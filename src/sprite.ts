/**
 * Mood rows in the sheet: working=0, waiting=1, done=2, idle=3.
 *
 * Slicing is not here — the sheet's rows and columns are now detected by `petpack.sliceSheet()`
 * from alpha gaps (a hardcoded 8×9 would slice a single image into transparent fragments).
 */
export const MOOD_ROWS = { working: 0, waiting: 1, done: 2, idle: 3 } as const;
