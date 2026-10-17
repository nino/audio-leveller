/** Text formatting for the window. Pure, so it is tested without a window. */

export function basename(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** "1 segment", "2 segments", "1 silence", "3 silences". */
export function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

export const signed = (n: number): string => `${n >= 0 ? "+" : ""}${n.toFixed(1)}`;

/** Decimals implied by a slider's step, so 0.05 doesn't read as "0.1". */
export function formatNumber(value: number, step: number): string {
  const decimals = step >= 1 ? 0 : step >= 0.1 ? 1 : step >= 0.01 ? 2 : 3;
  return value.toFixed(decimals);
}

export function formatValue(value: number, step: number, unit: string): string {
  return unit ? `${formatNumber(value, step)} ${unit}` : formatNumber(value, step);
}
