--[[
  Offline validation of the REAPER script's DSP (no REAPER required).

  Run:  lua reaper/test_dsp.lua

  Builds a synthetic signal (loud tone | silence | quiet tone), runs the
  analyser, applies the resulting gain envelope, and re-measures each segment —
  each should land near the -23 LUFS target. Also sanity-checks the gain signs.
]]

-- Load the module half of the script (reaper is nil here, so it just returns M).
local here = (arg and arg[0]) and arg[0]:gsub("[^/\\]*$", "") or ""
local M = dofile(here .. "audio_leveller.lua")

local fs = 48000
local failures = 0
local function check(name, cond)
  print((cond and "  ok   " or "  FAIL ") .. name)
  if not cond then failures = failures + 1 end
end

-- ── signal generators ──
local function sine(freq, sec, amp)
  local n = math.floor(sec * fs)
  local t = {}
  local w = 2 * math.pi * freq / fs
  for i = 0, n - 1 do t[i + 1] = amp * math.sin(w * i) end
  return t
end

local function noise(sec, amp)
  local n = math.floor(sec * fs)
  local t = {}
  local seed = 12345
  for i = 1, n do
    seed = (seed * 1103515245 + 12345) % 2147483648
    t[i] = ((seed / 2147483648) * 2 - 1) * amp
  end
  return t
end

local function concat(...)
  local out = {}
  for _, part in ipairs({ ... }) do
    for i = 1, #part do out[#out + 1] = part[i] end
  end
  return out
end

-- Loud 300 Hz tone | 1.5 s near-silence | quiet 500 Hz tone.
local signal = concat(sine(300, 3, 0.6), noise(1.5, 0.0002), sine(500, 3, 0.05))
local channels = { signal }

print(string.format("signal: %d samples (%.1fs @ %dHz)", #signal, #signal / fs, fs))

-- Target -23 exactly (disable the envelope-range auto-fit) so we can verify the
-- leveling maths directly.
local result = M.analyze(channels, fs, { maxBoostDb = 1000 })
print(string.format("integrated %.1f LUFS, floor %.1f, threshold %.1f",
  result.integratedLufs, result.floorLufs, result.thresholdLufs))
for i, s in ipairs(result.segments) do
  print(string.format("  seg #%d  %.2f-%.2fs  %.1f LUFS  gain %+.1f dB  (%s)",
    i, s.start / fs, s.stop / fs, s.lufs, s.gainDb, s.isSpeech and "speech" or "room"))
end

-- Expectations about the analysis itself.
check("found exactly one silence", #result.silences == 1)
check("found exactly two segments", #result.segments == 2)
check("loud segment is cut (gain < 0)", result.segments[1].gainDb < 0)
check("quiet segment is boosted (gain > 0)", result.segments[2].gainDb > 0)

-- Apply the gain envelope and re-measure each segment in the OUTPUT.
local out = {}
for i = 1, #signal do
  out[i] = signal[i] * (10 ^ (M.gainDbAt(result.breakpoints, i - 1) / 20))
end
local outWinMS, outNumWin, outWinLen = M.windowMeanSquares({ out }, fs, M.defaults.windowSec)

local function measureSegment(s)
  -- Measure the core of the segment, away from the ramp edges.
  local pad = math.floor(0.4 * fs)
  local w0 = math.min(outNumWin, math.floor((s.start + pad) / outWinLen) + 1)
  local w1 = math.min(outNumWin, math.floor((s.stop - pad - 1) / outWinLen) + 1)
  return M.integratedLoudness(outWinMS, w0, w1)
end

for i, s in ipairs(result.segments) do
  local measured = measureSegment(s)
  print(string.format("  after leveling: seg #%d = %.2f LUFS", i, measured))
  check(string.format("segment #%d within 1 LU of -23", i), math.abs(measured - (-23)) <= 1.0)
end

-- Envelope-range auto-fit: with a +6 dB boost ceiling, the quiet segment can't
-- reach -23, so the whole target should drop and no boost should exceed +6 dB.
print("")
local fitted = M.analyze(channels, fs, { maxBoostDb = 6 })
print(string.format("auto-fit: target %.1f → effective %.1f LUFS",
  fitted.targetLufs, fitted.effectiveTargetLufs))
check("auto-fit lowered the target below -23", fitted.effectiveTargetLufs < -23)
local maxGain = -math.huge
for _, s in ipairs(fitted.segments) do
  if s.isSpeech and s.gainDb > maxGain then maxGain = s.gainDb end
end
check("no speech boost exceeds +6 dB after auto-fit", maxGain <= 6.0 + 1e-6)

print("")
if failures == 0 then
  print("ALL PASS")
  os.exit(0)
else
  print(failures .. " FAILURE(S)")
  os.exit(1)
end
