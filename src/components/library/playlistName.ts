/**
 * What to call a playlist on screen.
 *
 * A built-in list is named by the catalogue, not by its database row. The migration seeds "Watch
 * Later" and "Favorites" in English so the rows exist from the first query with no startup ordering
 * to get wrong, but the display language may not be English — so the stored name is a key, not a
 * label.
 *
 * The mapping is an explicit switch rather than a key built by interpolating the slug. A
 * `library.playlist.${slug}` template would compile against any string and produce a blank label at
 * runtime the moment a catalogue entry was renamed; a switch makes that a build failure.
 */

import type { TranslationKey } from '@/i18n';
import type { LocalPlaylist } from '@/types/domain';

/** The name to render for `list`, translated when it is a built-in one. */
export function playlistName(
  list: Pick<LocalPlaylist, 'name' | 'is_system'>,
  translate: (key: TranslationKey) => string,
): string {
  if (list.is_system !== true) return list.name;

  switch (list.name.toLowerCase().replace(/\s+/gu, '_')) {
    case 'watch_later':
      return translate('library.playlist.watch_later');
    case 'favorites':
      return translate('library.playlist.favorites');
    default:
      // A built-in list the catalogue does not know about. Its stored name is a better answer than
      // an empty label, and this is the branch a future system playlist lands in until it is added.
      return list.name;
  }
}
