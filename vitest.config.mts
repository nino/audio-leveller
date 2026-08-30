import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["test/**/*.test.ts"],
    environment: "node",
    // Several of these tests are real DSP runs – WPE dereverb over a few
    // seconds of audio, DeepFilterNet inference stitched across chunks – and
    // they take four to five seconds on a CI runner. The 5 s default was never
    // sized for that: the slowest, `dereverb (WPE) > is deterministic`, last
    // passed on main with 78 ms to spare. That is a coin toss, not a pass, and
    // the next thing to cost a runner a moment tips the whole file red. Twenty
    // seconds is four times the slowest honest case, and still short enough
    // that something genuinely wedged is reported rather than waited on.
    testTimeout: 20_000,
  },
});
