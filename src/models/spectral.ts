/**
 * The classical backend: spectral suppression, always available.
 *
 * No weights, no native dependency, no download – so this is what runs by
 * default and what every eval baseline is measured against. See
 * `dsp/denoise.ts` for the algorithm and for why it is deliberately modest.
 */

import { denoise, DEFAULT_DENOISE_OPTIONS } from "../dsp/denoise";
import type { DenoiseBackend, DenoiseRequest, DenoiseResponse } from "./backend";

export const spectralBackend: DenoiseBackend = {
  name: "spectral",
  description: "Wiener suppression with a decision-directed SNR estimate (no model, no download)",

  available: () => true,
  unavailableReason: () => null,

  process(request: DenoiseRequest): DenoiseResponse {
    const result = denoise(request.channels, request.pauses, {
      ...DEFAULT_DENOISE_OPTIONS,
      reductionDb: request.reductionDb,
    });

    return {
      channels: result.channels,
      info: {
        noiseProfileFrames: result.profile.frames,
        // Worth surfacing: a profile from detected pauses is a direct
        // measurement, one from minimum statistics is an inference.
        noiseProfileFromPauses: result.profile.fromPauses,
        meanNoiseGainDb: result.meanNoiseGainDb,
        meanSpeechGainDb: result.meanSpeechGainDb,
      },
    };
  },
};
