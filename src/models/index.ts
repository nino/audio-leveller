import { registerBackend } from "./backend";
import { spectralBackend } from "./spectral";
import { onnxBackend } from "./onnx";

registerBackend(spectralBackend);
registerBackend(onnxBackend);

/**
 * Preference order. The model backend wins when its weights are present and
 * verify; otherwise the classical one runs and the report says why.
 */
export const DEFAULT_BACKEND_PREFERENCE = ["onnx", "spectral"];

export * from "./backend";
export { spectralBackend } from "./spectral";
export {
  onnxBackend,
  MODELS,
  modelDirectory,
  modelPath,
  modelFilePath,
  modelSize,
  runtimeAvailable,
  verifyChecksum,
  verifyModel,
} from "./onnx";
export type { ModelFile, ModelSpec, OnnxInfo } from "./onnx";
