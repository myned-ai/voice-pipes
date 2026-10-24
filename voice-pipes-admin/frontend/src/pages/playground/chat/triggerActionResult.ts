/** Result the command card sends with Complete while the box is untouched. */
export const DEFAULT_COMPLETED_RESULT = '{"status": "ok"}';

/** Result the card sends with Fail while the box is untouched. The model is
 *  told the call failed; a payload saying "ok" next to that is a
 *  contradiction it tends to resolve in favour of the payload. */
export const DEFAULT_FAILED_RESULT = '{"status": "error", "error": "failed from the Playground"}';

/** The JSON text a terminal button sends: what the tester typed, or, if
 *  they left the box alone, the default for that outcome. */
export function resultTextFor(phase: "completed" | "failed", text: string, edited: boolean): string {
  if (edited) return text;
  return phase === "failed" ? DEFAULT_FAILED_RESULT : DEFAULT_COMPLETED_RESULT;
}
