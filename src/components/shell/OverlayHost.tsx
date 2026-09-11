/**
 * Renders whichever overlay the UI store says is open.
 *
 * The store has declared an `Overlay` union since the beginning and nothing ever rendered it, so
 * `openOverlay` set state that no pixel reflected — a dialog system that existed entirely on paper.
 * This is the missing half.
 *
 * ## One host, not a dialog per caller
 *
 * A dialog opened from a card, a menu and a keyboard shortcut is the same dialog. Mounting it at
 * the root means the caller's own unmount cannot take the dialog with it — a menu that closes when
 * you click "Add to playlist" would otherwise close the thing it just opened.
 *
 * ## Only the kinds something can open
 *
 * `commandPalette` and `shortcuts` are declared in the union but nothing opens them and neither
 * screen exists yet, so neither is handled here. That is deliberate: a case rendering an empty
 * panel would be a feature claiming to exist. They fall through to nothing until they are
 * built, and the exhaustive `kind` switch will point here when they are.
 */

import { X } from 'lucide-react';
import { useEffect, useRef, useState, type ReactNode } from 'react';

import { playlistName } from '@/components/library/playlistName';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import type { TranslationKey } from '@/i18n';
import { useTranslation } from '@/i18n/context';
import { invoke } from '@/services/ipc';
import { useUiStore, type Overlay } from '@/stores/ui';
import type { LocalPlaylist, VideoSummary } from '@/types/domain';

/** How many playlists the "add to" list will show before it scrolls. */
const ADD_LIST_MAX_HEIGHT = '18rem';

/** The open overlay, or nothing. */
export function OverlayHost(): ReactNode {
  const overlay = useUiStore((state) => state.overlay);
  const close = useUiStore((state) => state.closeOverlay);

  if (overlay.kind === 'none') return null;

  return (
    <Dialog onClose={close} overlay={overlay}>
      {overlay.kind === 'createPlaylist' && <CreatePlaylist onClose={close} />}
      {overlay.kind === 'renamePlaylist' && (
        <RenamePlaylist id={overlay.id} currentName={overlay.currentName} onClose={close} />
      )}
      {overlay.kind === 'addToPlaylist' && <AddToPlaylist video={overlay.video} onClose={close} />}
      {overlay.kind === 'confirm' && <Confirm overlay={overlay} onClose={close} />}
    </Dialog>
  );
}

/** Title for the open overlay, so the panel is labelled for assistive technology. */
function titleKeyFor(overlay: Overlay): TranslationKey {
  switch (overlay.kind) {
    case 'createPlaylist':
      return 'library.newPlaylist';
    case 'renamePlaylist':
      return 'library.renamePlaylist';
    case 'addToPlaylist':
      return 'library.addToPlaylist';
    case 'confirm':
      return overlay.titleKey;
    default:
      return 'app.name';
  }
}

/**
 * What Tab is allowed to land on inside a dialog.
 *
 * Deliberately not `*` with a `tabIndex` check: this is the set the browser itself would visit, and
 * anything with `tabindex="-1"` — the panel included — is reachable programmatically but is not a
 * stop on the ring.
 */
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), ' +
  'select:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** The backdrop and panel every overlay sits in. */
function Dialog({
  overlay,
  onClose,
  children,
}: {
  overlay: Overlay;
  onClose: () => void;
  children: ReactNode;
}): ReactNode {
  const t = useTranslation();
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onClose();
        return;
      }

      // Tab is kept inside the panel, which is the half of `aria-modal="true"` the attribute does
      // not actually do. It tells assistive technology that everything behind is inert; it does
      // nothing to the tab ring. So a few presses of Tab walked out of the dialog and into the
      // sidebar and the feed underneath — still reachable, still clickable, with a modal drawn
      // over them and no way back except the pointer this is meant to be an alternative to.
      if (event.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;

      const focusable = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
        // `offsetParent` is null for anything `display: none`, which is how a hidden control in a
        // dialog that swaps its contents would otherwise capture the focus ring.
        (element) => element.offsetParent !== null || element === document.activeElement,
      );
      if (focusable.length === 0) {
        // Nothing to move between; keep focus on the panel rather than letting it escape.
        event.preventDefault();
        panel.focus();
        return;
      }

      const first = focusable[0];
      const last = focusable.at(-1);
      if (!first || !last) return;
      const active = document.activeElement;

      // Wrapping in both directions, and from the panel itself, which is where focus sits when the
      // dialog has no field to open into.
      if (event.shiftKey && (active === first || active === panel)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || active === panel)) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [onClose]);

  // Focus moves into the panel on open, so the keyboard is where the eye is.
  //
  // Fields only, and never a button. `querySelector('input, button, ...')` returns the first match
  // in *document order*, and the close control is above the form — so focus landed on the X, and
  // pressing space, which activates a focused button, closed the dialog. Typing any playlist name
  // with a space in it dismissed the thing you were typing into.
  //
  // Falling back to the panel rather than to some other control is deliberate: the panel carries
  // `tabIndex={-1}`, so Escape and Tab still work from it, and nothing can be triggered by a
  // keystroke meant as text.
  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) return undefined;

    // Remembered before focus moves, restored when the dialog goes.
    //
    // Without this, closing a dialog dropped focus on `<body>`: the next Tab started from the top
    // of the document, so someone who opened "Add to playlist" from a card halfway down the feed
    // was returned to the skip link and had to walk all the way back. The control that opened the
    // dialog is where they were, and it is where they belong afterwards.
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    const field = panel.querySelector<HTMLElement>('input, textarea, select');
    (field ?? panel).focus();

    return () => {
      // Only if it is still there. A dialog that deletes the playlist row that opened it leaves an
      // opener that is no longer in the document, and focusing a detached node silently does
      // nothing — which is the `<body>` case again, by another route.
      if (opener?.isConnected) opener.focus();
    };
    // Re-run when the dialog changes what it is showing. The host keeps one `Dialog` mounted and
    // swaps its contents, so with an empty list this ran once ever: moving from one overlay to
    // another left focus on the element the previous one had, or on `<body>` once that element was
    // gone — and a dialog nothing is focused inside is one the keyboard cannot reach.
  }, [overlay.kind]);

  return (
    <div
      className="fixed inset-0 z-[2000] grid place-items-center p-6"
      // Dismisses on the backdrop but not on the panel: the check is that the press *started* and
      // ended on the backdrop itself, so a drag that begins inside the panel and releases outside
      // does not close it — which is how a text selection turns into a lost dialog.
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      style={{ background: 'rgba(0,0,0,0.6)' }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={t.t(titleKeyFor(overlay))}
        tabIndex={-1}
        // Capped and scrollable. The panel grew to whatever its contents needed and the grid
        // centred it, so on a short window — a 768px laptop, or any window dragged small — a long
        // playlist list pushed the Cancel and Save buttons off the bottom of the screen with no way
        // to reach them.
        className="animate-dialog-in bg-surface border-border max-h-[85vh] w-full max-w-md overflow-y-auto rounded-xl border p-5 shadow-2xl outline-none"
      >
        <div className="mb-4 flex items-start justify-between gap-4">
          <h2 className="text-text text-base font-medium">{t.t(titleKeyFor(overlay))}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={t.t('app.close')}
            className="transition-surface text-text-muted hover:bg-surface-hover hover:text-text -mt-1 -mr-1 grid size-8 shrink-0 place-items-center rounded-full"
          >
            <X size={18} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}

/** A text field plus Cancel/Confirm, shared by create and rename. */
function NameForm({
  initial,
  confirmKey,
  onSubmit,
  onClose,
}: {
  initial: string;
  confirmKey: TranslationKey;
  onSubmit: (name: string) => Promise<void>;
  onClose: () => void;
}): ReactNode {
  const t = useTranslation();
  const [name, setName] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const trimmed = name.trim();

  const submit = () => {
    // The storage layer refuses a blank name too; this stops the round trip that would only come
    // back to say so.
    if (trimmed.length === 0 || busy) return;
    setBusy(true);
    setFailed(false);
    void onSubmit(trimmed).then(
      () => {
        setBusy(false);
        onClose();
      },
      () => {
        // Closed only on success. An earlier version closed in a `finally`, so a failed create
        // dismissed the dialog and left the screen exactly as it had been — the user pressed the
        // button, everything vanished, and nothing had happened. Staying open with the typed name
        // intact is the difference between a retry and a mystery.
        setBusy(false);
        setFailed(true);
      },
    );
  };

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <input
        value={name}
        onChange={(event) => {
          setName(event.target.value);
        }}
        maxLength={512}
        aria-label={t.t('library.playlistName')}
        placeholder={t.t('library.playlistName')}
        className="border-border bg-bg text-text focus-visible:border-border-focus w-full rounded-md border px-3 py-2 text-sm outline-none"
      />
      {failed && (
        <p role="alert" className="text-danger mt-2 text-xs">
          {t.t('error.generic')}
        </p>
      )}

      <div className="mt-5 flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="transition-surface text-text hover:bg-surface-hover rounded-full px-4 py-2 text-sm"
        >
          {t.t('app.cancel')}
        </button>
        <button
          type="submit"
          disabled={trimmed.length === 0 || busy}
          className="transition-surface bg-text text-bg rounded-full px-4 py-2 text-sm font-medium disabled:opacity-40"
        >
          {t.t(confirmKey)}
        </button>
      </div>
    </form>
  );
}

function CreatePlaylist({ onClose }: { onClose: () => void }): ReactNode {
  const bump = useUiStore((state) => state.notePlaylistsChanged);
  return (
    <NameForm
      initial=""
      confirmKey="library.create"
      onClose={onClose}
      onSubmit={async (name) => {
        await invoke('create_playlist', { name, description: null });
        bump();
      }}
    />
  );
}

function RenamePlaylist({
  id,
  currentName,
  onClose,
}: {
  // The store's own shape: a plain row identifier, which is also what the command takes.
  id: number;
  currentName: string;
  onClose: () => void;
}): ReactNode {
  const bump = useUiStore((state) => state.notePlaylistsChanged);
  return (
    <NameForm
      initial={currentName}
      confirmKey="app.rename"
      onClose={onClose}
      onSubmit={async (name) => {
        await invoke('rename_playlist', { playlistId: id, name });
        bump();
      }}
    />
  );
}

/**
 * Membership of every playlist at once, toggled in place.
 *
 * Which playlists already hold the video is one query rather than one per playlist, and each toggle
 * writes immediately — there is no Save button, because there is nothing to save: the list either
 * contains the video or it does not, and a staged edit would only invent a state to get wrong.
 */
function AddToPlaylist({
  video,
  onClose,
}: {
  video: VideoSummary;
  onClose: () => void;
}): ReactNode {
  const t = useTranslation();
  const bump = useUiStore((state) => state.notePlaylistsChanged);
  const openOverlay = useUiStore((state) => state.openOverlay);

  const loaded = useAsyncResource(`add-to-playlist:${video.id}`, async () => {
    const [playlists, holding] = await Promise.all([
      invoke('get_playlists', undefined),
      invoke('playlists_containing', { videoId: video.id }),
    ]);
    return { playlists, holding: new Set<number>(holding) };
  });

  // Local overrides so a toggle shows immediately rather than after a refetch. Keyed by playlist,
  // and only consulted for the ones actually touched — the fetched answer stays authoritative for
  // everything else.
  const [changed, setChanged] = useState<ReadonlyMap<number, boolean>>(new Map());

  const toggle = (list: LocalPlaylist, isIn: boolean) => {
    setChanged((current) => new Map(current).set(list.id, !isIn));
    const call = isIn
      ? invoke('remove_from_playlist', { playlistId: list.id, videoId: video.id })
      : invoke('add_to_playlist', { playlistId: list.id, video });
    void call
      .then(() => {
        bump();
      })
      .catch(() => {
        // Put the row back the way it was: the write did not happen, and showing it as though it
        // did is the one outcome worse than the failure.
        setChanged((current) => new Map(current).set(list.id, isIn));
      });
  };

  if (loaded.error && !loaded.data) {
    return <p className="text-text-muted text-sm">{t.t('error.generic')}</p>;
  }

  const playlists = loaded.data?.playlists ?? [];

  return (
    <>
      <div className="overflow-y-auto" style={{ maxHeight: ADD_LIST_MAX_HEIGHT }}>
        {playlists.map((list) => {
          const isIn = changed.get(list.id) ?? loaded.data?.holding.has(list.id) ?? false;
          return (
            <label
              key={list.id}
              className="transition-surface hover:bg-surface-hover flex cursor-pointer items-center gap-3 rounded-md px-2 py-2"
            >
              <input
                type="checkbox"
                checked={isIn}
                onChange={() => {
                  toggle(list, isIn);
                }}
                className="accent-accent size-4 shrink-0"
              />
              <span className="text-text min-w-0 flex-1 truncate text-sm">
                {playlistName(list, t.t)}
              </span>
              <span className="text-text-muted shrink-0 text-xs">{list.item_count}</span>
            </label>
          );
        })}
        {playlists.length === 0 && !loaded.loading && (
          <p className="text-text-muted px-2 py-4 text-sm">{t.t('library.empty.playlists')}</p>
        )}
      </div>

      <div className="border-border mt-4 flex justify-between gap-2 border-t pt-4">
        <button
          type="button"
          onClick={() => {
            openOverlay({ kind: 'createPlaylist' });
          }}
          className="transition-surface text-text hover:bg-surface-hover rounded-full px-4 py-2 text-sm"
        >
          {t.t('library.newPlaylist')}
        </button>
        <button
          type="button"
          onClick={onClose}
          className="transition-surface bg-text text-bg rounded-full px-4 py-2 text-sm font-medium"
        >
          {t.t('app.done')}
        </button>
      </div>
    </>
  );
}

function Confirm({
  overlay,
  onClose,
}: {
  overlay: Extract<Overlay, { kind: 'confirm' }>;
  onClose: () => void;
}): ReactNode {
  const t = useTranslation();
  return (
    <>
      <p className="text-text-muted text-sm">{t.t(overlay.bodyKey)}</p>
      <div className="mt-5 flex justify-end gap-2">
        <button
          type="button"
          onClick={onClose}
          className="transition-surface text-text hover:bg-surface-hover rounded-full px-4 py-2 text-sm"
        >
          {t.t('app.cancel')}
        </button>
        <button
          type="button"
          onClick={() => {
            overlay.onConfirm();
            onClose();
          }}
          className="transition-surface border-danger text-danger hover:bg-danger rounded-full border px-4 py-2 text-sm font-medium hover:text-white"
        >
          {t.t(overlay.confirmKey)}
        </button>
      </div>
    </>
  );
}
