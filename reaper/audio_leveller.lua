--[[
  Audio Leveller for REAPER  –  non-destructive loudness leveller.

  For every selected media item it analyses the take's audio (BS.1770 / EBU
  R128 loudness), finds the silences, and writes a TAKE VOLUME ENVELOPE that
  normalises each speech segment to the target loudness (-23 LUFS by default),
  ramping the gain linearly across the silences in between. Nothing is
  rendered or overwritten – it's all automation.

  This is a port of the standalone Audio Leveller tool. The leveling maths are
  cross-checked against that tool (see reaper/test_dsp.lua).

  INSTALL:  Actions → Show action list → New action → Load ReaScript…
            pick this file. Select item(s), run the action.

  The pure-DSP half of this file (everything above the "REAPER GLUE" banner)
  has no dependency on REAPER, so it can be loaded and unit-tested in a plain
  Lua interpreter – when `reaper` is not defined the file just returns the
  module table instead of running.
]]

local M = {}

-- ─────────────────────────────────────────────────────────────────────────
-- Options (defaults match the standalone tool)
-- ─────────────────────────────────────────────────────────────────────────
M.defaults = {
  targetLufs   = -23.0,  -- desired FINAL loudness of the speech
  -- The envelope levels to (targetLufs - makeupGainDb); this many dB of makeup
  -- is then applied via the item volume so the envelope only ever CUTS (which
  -- has unlimited range) and never fights REAPER's limited boost range. With
  -- the defaults the envelope targets -35 LUFS and +12 dB makeup lands it at -23.
  makeupGainDb = 12.0,
  -- A segment counts as speech only if it's within this many dB of the whole
  -- file's integrated loudness. Stops quiet room / handling noise between
  -- silences from being treated (and boosted) as speech.
  maxSpeechDropDb = 18.0,
  -- Per-segment boost cap, to fit REAPER's take-volume-envelope range
  -- (Preferences → Envelope Display → "Volume envelope range", often +6 dB).
  maxBoostDb   = 6.0,
  minSilenceSec = 1.0,
  windowSec    = 0.1,   -- analysis window; also the block unit for gating
  fraction     = 0.25,  -- silence threshold: floor + fraction*(integrated-floor)
  maxGainDb    = 30.0,  -- cut cap
}

-- ─────────────────────────────────────────────────────────────────────────
-- Small maths helpers
-- ─────────────────────────────────────────────────────────────────────────
local NEG_INF = -math.huge

local function log10(x) return math.log(x) / math.log(10) end

-- Loudness (LUFS) from a weighted mean square.
local function loudness(ms)
  if ms <= 0 then return NEG_INF end
  return -0.691 + 10 * log10(ms)
end

local function mean(t, from, to)
  from = from or 1
  to = to or #t
  if to < from then return 0 end
  local s = 0
  for i = from, to do s = s + t[i] end
  return s / (to - from + 1)
end

-- p-th percentile (0..100) of an already-collected list of finite numbers.
local function percentile(values, p)
  local n = #values
  if n == 0 then return NEG_INF end
  local sorted = {}
  for i = 1, n do sorted[i] = values[i] end
  table.sort(sorted)
  local idx = math.floor((p / 100) * (n - 1) + 0.5) + 1
  if idx < 1 then idx = 1 elseif idx > n then idx = n end
  return sorted[idx]
end

-- ─────────────────────────────────────────────────────────────────────────
-- K-weighting (ITU-R BS.1770) biquad coefficients – derived per sample rate.
-- ─────────────────────────────────────────────────────────────────────────
local function shelvingFilter(fs)
  local f0, G, Q = 1681.9744509555319, 3.9998438533940324, 0.7071752369554193
  local K = math.tan(math.pi * f0 / fs)
  local Vh = 10 ^ (G / 20)
  local Vb = Vh ^ 0.4996667741545416
  local a0 = 1 + K / Q + K * K
  return {
    b0 = (Vh + Vb * K / Q + K * K) / a0,
    b1 = (2 * (K * K - Vh)) / a0,
    b2 = (Vh - Vb * K / Q + K * K) / a0,
    a1 = (2 * (K * K - 1)) / a0,
    a2 = (1 - K / Q + K * K) / a0,
  }
end

local function highPassFilter(fs)
  local f0, Q = 38.13547087602444, 0.5003270373238773
  local K = math.tan(math.pi * f0 / fs)
  local a0 = 1 + K / Q + K * K
  return {
    b0 = 1, b1 = -2, b2 = 1,
    a1 = (2 * (K * K - 1)) / a0,
    a2 = (1 - K / Q + K * K) / a0,
  }
end

local function newBiquadState() return { x1 = 0, x2 = 0, y1 = 0, y2 = 0 } end

local function biquadStep(c, s, x)
  local y = c.b0 * x + c.b1 * s.x1 + c.b2 * s.x2 - c.a1 * s.y1 - c.a2 * s.y2
  s.x2 = s.x1; s.x1 = x
  s.y2 = s.y1; s.y1 = y
  return y
end

-- BS.1770 channel weight (1-based channel index). Surround (5th+) weight 1.41.
local function channelWeight(ch) if ch >= 4 then return 1.41 else return 1.0 end end

-- ─────────────────────────────────────────────────────────────────────────
-- Stage 1: per-window K-weighted mean square.
-- `channels` is an array (1..nch) of arrays (1..n) of floats in [-1, 1].
-- Returns windowMS[1..numWin], numWin, winLen.
-- ─────────────────────────────────────────────────────────────────────────
function M.windowMeanSquares(channels, fs, windowSec)
  local nch = #channels
  local n = #channels[1]
  local winLen = math.max(1, math.floor(windowSec * fs + 0.5))
  local numWin = math.floor(n / winLen)

  local sh, hp = shelvingFilter(fs), highPassFilter(fs)
  local st1, st2 = {}, {}
  for c = 1, nch do st1[c] = newBiquadState(); st2[c] = newBiquadState() end

  local windowMS = {}
  for w = 1, numWin do
    local base = (w - 1) * winLen
    local sum = 0
    for c = 1, nch do
      local ch = channels[c]
      local s1, s2 = st1[c], st2[c]
      local acc = 0
      for i = 1, winLen do
        local x = ch[base + i]
        local y = biquadStep(hp, s2, biquadStep(sh, s1, x))
        acc = acc + y * y
      end
      sum = sum + channelWeight(c) * (acc / winLen)
    end
    windowMS[w] = sum
  end
  return windowMS, numWin, winLen
end

-- ─────────────────────────────────────────────────────────────────────────
-- Gated integrated loudness (LUFS) over the window range [w0, w1] (inclusive).
-- 400 ms blocks = 4 windows, 100 ms hop = 1 window, with the BS.1770 absolute
-- (-70) and relative (-10 LU) gates. Falls back to plain loudness for ranges
-- shorter than one block.
-- ─────────────────────────────────────────────────────────────────────────
function M.integratedLoudness(windowMS, w0, w1)
  if w1 < w0 then return NEG_INF end

  local blocks = {}
  for s = w0, w1 - 3 do
    blocks[#blocks + 1] = (windowMS[s] + windowMS[s + 1] + windowMS[s + 2] + windowMS[s + 3]) / 4
  end
  if #blocks == 0 then
    return loudness(mean(windowMS, w0, w1)) -- range shorter than a block
  end

  local absKept = {}
  for _, m in ipairs(blocks) do if loudness(m) >= -70 then absKept[#absKept + 1] = m end end
  if #absKept == 0 then return NEG_INF end

  local relThresh = loudness(mean(absKept)) - 10
  local relKept = {}
  for _, m in ipairs(blocks) do
    if loudness(m) >= -70 and loudness(m) >= relThresh then relKept[#relKept + 1] = m end
  end
  if #relKept == 0 then return NEG_INF end

  return loudness(mean(relKept))
end

-- ─────────────────────────────────────────────────────────────────────────
-- Full analysis → gain envelope breakpoints (in SAMPLES).
-- Returns a table:
--   { fs, n, thresholdLufs, floorLufs, integratedLufs,
--     silences = { {start=, stop=, mid=}, ... },
--     segments = { {start=, stop=, lufs=, gainDb=, isSpeech=}, ... },
--     breakpoints = { {at=<sample>, db=<gain dB>}, ... } }
-- ─────────────────────────────────────────────────────────────────────────
function M.analyze(channels, fs, opts)
  opts = opts or {}
  local o = {}
  for k, v in pairs(M.defaults) do o[k] = opts[k] ~= nil and opts[k] or v end

  local n = #channels[1]
  local windowMS, numWin, winLen = M.windowMeanSquares(channels, fs, o.windowSec)

  -- Per-window loudness + threshold.
  local winLoud = {}
  local finite = {}
  for w = 1, numWin do
    local l = loudness(windowMS[w])
    winLoud[w] = l
    if l > NEG_INF then finite[#finite + 1] = l end
  end
  local integrated = M.integratedLoudness(windowMS, 1, numWin)
  local floor = percentile(finite, 10)
  local threshold = floor + o.fraction * (integrated - floor)

  -- Silence runs: consecutive sub-threshold windows, at least minSilence long.
  local minWin = math.max(1, math.ceil(o.minSilenceSec / o.windowSec))
  local silences = {}
  local runStart = nil
  for w = 1, numWin + 1 do
    local silent = (w <= numWin) and (winLoud[w] < threshold)
    if silent and not runStart then
      runStart = w
    elseif not silent and runStart then
      if (w - runStart) >= minWin then
        local startS = (runStart - 1) * winLen
        local stopS = math.min(n, (w - 1) * winLen)
        silences[#silences + 1] = { start = startS, stop = stopS, mid = math.floor((startS + stopS) / 2) }
      end
      runStart = nil
    end
  end

  -- Segments: file edges + silence midpoints.
  local bounds = { 0 }
  for _, s in ipairs(silences) do bounds[#bounds + 1] = s.mid end
  bounds[#bounds + 1] = n

  -- The envelope levels to (target - makeup); makeup is added later on the item
  -- volume. Keeping the envelope target below the speech level means it only
  -- cuts, so it never hits REAPER's limited boost range.
  local envTarget = o.targetLufs - o.makeupGainDb
  local speechFloor = integrated - o.maxSpeechDropDb

  -- Pass 1: segment loudness + speech classification. A segment is speech only
  -- if it's above the silence threshold AND within maxSpeechDropDb of the whole
  -- file's integrated loudness (so quiet room between silences isn't "speech").
  local segLufs, isSpeech = {}, {}
  local anySpeech = false
  for i = 1, #bounds - 1 do
    local a, b = bounds[i], bounds[i + 1]
    local w0 = math.min(numWin, math.floor(a / winLen) + 1)
    local w1 = math.min(numWin, math.floor((b - 1) / winLen) + 1)
    local l = M.integratedLoudness(windowMS, w0, w1)
    segLufs[i] = l
    isSpeech[i] = (l > NEG_INF) and (l > threshold) and (l > speechFloor)
    if isSpeech[i] then anySpeech = true end
  end
  if not anySpeech then
    for i = 1, #segLufs do isSpeech[i] = (segLufs[i] > NEG_INF) end
  end

  -- Per-segment gain to the (fixed) envelope target, clamped to the envelope's
  -- boost/cut range.
  local speechGain = {}
  for i = 1, #segLufs do
    if segLufs[i] > NEG_INF then
      local g = envTarget - segLufs[i]
      if g > o.maxBoostDb then g = o.maxBoostDb elseif g < -o.maxGainDb then g = -o.maxGainDb end
      speechGain[i] = g
    else
      speechGain[i] = 0
    end
  end

  -- Non-speech segments inherit their nearest speech neighbour's gain.
  local gains = {}
  for i = 1, #speechGain do
    if isSpeech[i] then
      gains[i] = speechGain[i]
    else
      local best, bestDist = nil, math.huge
      for j = 1, #speechGain do
        if isSpeech[j] and math.abs(j - i) < bestDist then best, bestDist = j, math.abs(j - i) end
      end
      gains[i] = best and speechGain[best] or 0
    end
  end

  -- Breakpoints: constant across each segment body, linear ramp across silence.
  local bp = { { at = 0, db = gains[1] } }
  for i, s in ipairs(silences) do
    bp[#bp + 1] = { at = s.start, db = gains[i] }
    bp[#bp + 1] = { at = s.stop, db = gains[i + 1] }
  end
  bp[#bp + 1] = { at = n, db = gains[#gains] }

  local segments = {}
  for i = 1, #bounds - 1 do
    segments[i] = { start = bounds[i], stop = bounds[i + 1], lufs = segLufs[i], gainDb = gains[i], isSpeech = isSpeech[i] }
  end

  return {
    fs = fs, n = n,
    thresholdLufs = threshold, floorLufs = floor, integratedLufs = integrated,
    targetLufs = o.targetLufs, envelopeTargetLufs = envTarget, makeupGainDb = o.makeupGainDb,
    silences = silences, segments = segments, breakpoints = bp,
  }
end

-- Gain (in dB) of the piecewise-linear envelope at a given sample position.
function M.gainDbAt(bp, sample)
  for i = #bp, 1, -1 do
    if sample >= bp[i].at then
      if i == #bp then return bp[i].db end
      local a, b = bp[i], bp[i + 1]
      local span = b.at - a.at
      local t = span > 0 and (sample - a.at) / span or 0
      return a.db + t * (b.db - a.db)
    end
  end
  return bp[1].db
end

-- ═════════════════════════════════════════════════════════════════════════
-- REAPER GLUE  (only runs inside REAPER; not needed for the offline tests)
-- ═════════════════════════════════════════════════════════════════════════
local function runInReaper()
  -- Set to true to only print the analysis to the console (no envelope written).
  local DRY_RUN = false
  -- "Take: Toggle take volume envelope" – verify this ID in your REAPER via the
  -- Action list if the envelope isn't created (search that exact name).
  local TOGGLE_TAKE_VOL_ENV = 40693

  local function msg(s) reaper.ShowConsoleMsg(tostring(s) .. "\n") end

  local function readTakeSamples(take)
    local acc = reaper.CreateTakeAudioAccessor(take)
    local source = reaper.GetMediaItemTake_Source(take)
    local fs = reaper.GetMediaSourceSampleRate(source)
    local nch = reaper.GetMediaSourceNumChannels(source)
    local startT = reaper.GetAudioAccessorStartTime(acc)
    local endT = reaper.GetAudioAccessorEndTime(acc)
    local total = math.floor((endT - startT) * fs)
    if total < 1 or nch < 1 then reaper.DestroyAudioAccessor(acc); return nil end

    local channels = {}
    for c = 1, nch do channels[c] = {} end

    local blockFrames = 65536
    local buf = reaper.new_array(blockFrames * nch)
    local frame = 0
    while frame < total do
      local nframes = math.min(blockFrames, total - frame)
      reaper.GetAudioAccessorSamples(acc, fs, nch, startT + frame / fs, nframes, buf)
      for i = 0, nframes - 1 do
        local o = i * nch
        for c = 1, nch do channels[c][frame + i + 1] = buf[o + c] end
      end
      frame = frame + nframes
    end
    reaper.DestroyAudioAccessor(acc)
    return channels, fs, nch, total
  end

  local function getTakeVolEnvelope(take)
    local env = reaper.GetTakeEnvelopeByName(take, "Volume")
    if env then return env end
    reaper.Main_OnCommand(TOGGLE_TAKE_VOL_ENV, 0) -- creates the take volume env
    return reaper.GetTakeEnvelopeByName(take, "Volume")
  end

  -- Write the breakpoints (in samples) to the take volume envelope. One point
  -- per breakpoint – a flat level across each segment and a single straight
  -- ramp across each silence (REAPER interpolates linearly between the two).
  local function writeEnvelope(take, result, playrate)
    local env = getTakeVolEnvelope(take)
    if not env then msg("  ! could not create take volume envelope"); return false end

    local scaling = reaper.GetEnvelopeScalingMode(env)
    reaper.DeleteEnvelopePointRange(env, -math.huge, math.huge)

    local fs = result.fs
    for _, p in ipairs(result.breakpoints) do
      local itemTime = (p.at / fs) / playrate
      local val = reaper.ScaleToEnvelopeMode(scaling, 10 ^ (p.db / 20))
      reaper.InsertEnvelopePoint(env, itemTime, val, 0, 0, false, true)
    end
    reaper.Envelope_SortPoints(env)
    return true
  end

  -- ── main ──
  local numItems = reaper.CountSelectedMediaItems(0)
  if numItems == 0 then
    reaper.ShowMessageBox("Select at least one media item first.", "Audio Leveller", 0)
    return
  end

  reaper.Undo_BeginBlock()
  reaper.PreventUIRefresh(1)
  reaper.ClearConsole()

  local done, skipped = 0, 0
  for it = 0, numItems - 1 do
    local item = reaper.GetSelectedMediaItem(0, it)
    local take = reaper.GetActiveTake(item)
    if take and not reaper.TakeIsMIDI(take) then
      local _, name = reaper.GetSetMediaItemTakeInfo_String(take, "P_NAME", "", false)
      local playrate = reaper.GetMediaItemTakeInfo_Value(take, "D_PLAYRATE")
      if playrate <= 0 then playrate = 1 end

      local channels, fs = readTakeSamples(take)
      if channels then
        local result = M.analyze(channels, fs)
        msg(string.format("%s  (integrated %.1f LUFS, threshold %.1f, %d silence(s), %d segment(s))",
          name, result.integratedLufs, result.thresholdLufs, #result.silences, #result.segments))
        msg(string.format("  envelope target %.1f LUFS + %.1f dB item makeup → %.1f LUFS",
          result.envelopeTargetLufs, result.makeupGainDb, result.targetLufs))
        for i, s in ipairs(result.segments) do
          local l = (s.lufs > -math.huge) and string.format("%.1f LUFS", s.lufs) or "silent"
          msg(string.format("  #%d  %.2f-%.2fs  %s  %s  gain %+.1f dB",
            i, s.start / fs, s.stop / fs, l, s.isSpeech and "speech" or "room", s.gainDb))
        end
        if not DRY_RUN then
          if writeEnvelope(take, result, playrate) then
            -- Apply the makeup gain on the item volume (stacks with the take
            -- envelope), so the leveled speech lands at the final target.
            reaper.SetMediaItemInfo_Value(item, "D_VOL", 10 ^ (result.makeupGainDb / 20))
            done = done + 1
          end
        else
          done = done + 1
        end
      else
        skipped = skipped + 1
      end
    else
      skipped = skipped + 1
    end
  end

  reaper.PreventUIRefresh(-1)
  reaper.UpdateArrange()
  reaper.Undo_EndBlock("Audio Leveller: take volume automation", -1)
  msg(string.format("\nDone. %d item(s) leveled%s.", done, skipped > 0 and (", " .. skipped .. " skipped") or ""))
end

if reaper ~= nil then
  runInReaper()
  return M
else
  return M
end
