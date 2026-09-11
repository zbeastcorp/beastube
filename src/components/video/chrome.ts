/** Values shared by the watch player and the Shorts feed, which draw the same chrome. */

/**
 * Height of the embed's own top and bottom bands, in the embed's pixels.
 *
 * Both surfaces hide YouTube's title bar and control strip by laying the frame out taller than the
 * visible window and shifting it, and both need the same number to do it. How each one applies the
 * crop differs, and is explained where it is applied.
 */
export const EMBED_CHROME_CROP_PX = 64;

/** How long the pointer must rest before overlaid controls fade, as YouTube's do. */
export const CHROME_IDLE_MS = 2600;
