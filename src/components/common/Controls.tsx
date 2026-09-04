/**
 * Form primitives.
 *
 * Every settings control is built from these, so accessibility and keyboard behaviour are decided
 * once rather than per screen. Each is a real form element underneath — a `<button role="switch">`,
 * a `<select>`, an `<input type="range">` — because reimplementing them with divs means
 * reimplementing focus, keyboard handling and screen-reader semantics, and getting some of it
 * wrong (§64).
 *
 * Rows carry an optional hint. A setting whose consequence is not obvious from its label gets one:
 * "hardware acceleration" means nothing to most people until you say it is what to turn off when
 * the picture is black.
 */

import type { ChangeEvent, ReactNode } from 'react';
import { useId } from 'react';

/** A labelled row wrapping one control. */
export function SettingRow({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string;
  hint?: string;
  htmlFor?: string;
  children: ReactNode;
}): ReactNode {
  return (
    // Wraps. A row is a label on the left and a control on the right only while both fit; below
    // that the control drops to its own line rather than the label being squeezed to nothing or
    // the control being pushed past the panel's edge. `basis-64` is where that swap happens.
    <div className="border-border flex flex-wrap items-start justify-between gap-x-6 gap-y-3 border-b py-4 last:border-b-0">
      <div className="flex min-w-0 flex-[1_1_16rem] flex-col gap-1">
        <label htmlFor={htmlFor} className="text-text text-base">
          {label}
        </label>
        {/* An empty hint is "no hint", not an empty paragraph: callers pass one conditionally and
            a blank line under the label reads as a missing string. */}
        {hint !== undefined && hint !== '' && (
          <p className="text-text-muted max-w-prose text-xs">{hint}</p>
        )}
      </div>
      <div className="flex max-w-full flex-wrap items-center justify-end gap-2">{children}</div>
    </div>
  );
}

/** A settings section with a heading. */
export function SettingsSection({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}): ReactNode {
  return (
    <section className="mb-10">
      <h2 className="text-text mb-1 text-lg font-medium">{title}</h2>
      {description !== undefined && (
        <p className="text-text-muted mb-3 max-w-prose text-sm">{description}</p>
      )}
      <div className="bg-surface rounded-lg px-4">{children}</div>
    </section>
  );
}

/** An on/off switch. */
export function Switch({
  checked,
  onChange,
  label,
  id,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** Accessible name, used when the switch is not associated with a visible label. */
  label?: string;
  id?: string;
}): ReactNode {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => {
        onChange(!checked);
      }}
      className={[
        'relative h-6 w-11 shrink-0 rounded-full p-0 transition-colors duration-[var(--duration-fast)]',
        checked ? 'bg-accent' : 'bg-surface-active',
      ].join(' ')}
    >
      {/* `left-0` and the button's `p-0` are load-bearing: without an inset the knob is placed at
          its static position, which the user agent's default button padding shifts right — the knob
          then rides off the end of the track when it translates. */}
      <span
        className={[
          'absolute top-0.5 left-0 size-5 rounded-full bg-white shadow-sm',
          'transition-transform duration-[var(--duration-fast)] ease-[var(--ease-yt)]',
          checked ? 'translate-x-[1.375rem]' : 'translate-x-0.5',
        ].join(' ')}
      />
    </button>
  );
}

/** One option in a {@link Select}. */
export interface SelectOption<T extends string> {
  value: T;
  label: string;
}

/** A dropdown. */
export function Select<T extends string>({
  value,
  options,
  onChange,
  id,
  label,
}: {
  value: T;
  options: readonly SelectOption<T>[];
  onChange: (value: T) => void;
  id?: string;
  label?: string;
}): ReactNode {
  return (
    <select
      id={id}
      aria-label={label}
      value={value}
      onChange={(event: ChangeEvent<HTMLSelectElement>) => {
        onChange(event.target.value as T);
      }}
      className="border-border bg-bg text-text focus-visible:border-border-focus min-w-[10rem] rounded-md border px-3 py-2 text-sm outline-none"
    >
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

/** A numeric slider with its current value shown. */
export function Slider({
  value,
  min,
  max,
  step = 1,
  onChange,
  format,
  id,
  label,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (value: number) => void;
  /** Renders the current value; defaults to the raw number. */
  format?: (value: number) => string;
  id?: string;
  label?: string;
}): ReactNode {
  return (
    <div className="flex items-center gap-3">
      <input
        id={id}
        type="range"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => {
          onChange(Number(event.target.value));
        }}
        className="accent-accent w-40"
      />
      <span className="text-text-muted w-16 text-right font-mono text-xs">
        {format ? format(value) : value}
      </span>
    </div>
  );
}

/** A button that performs a destructive action, styled to look like one. */
export function DangerButton({
  onClick,
  children,
  disabled = false,
}: {
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}): ReactNode {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="transition-surface border-danger text-danger hover:bg-danger rounded-full border px-4 py-2 text-sm font-medium hover:text-white disabled:opacity-50"
    >
      {children}
    </button>
  );
}

/** A neutral secondary button. */
export function SecondaryButton({
  onClick,
  children,
  disabled = false,
}: {
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}): ReactNode {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="transition-surface bg-surface-translucent hover:bg-surface-translucent-hover text-text rounded-full px-4 py-2 text-sm font-medium disabled:opacity-50"
    >
      {children}
    </button>
  );
}

/** A read-only fact, for paths and measured values. */
export function ReadOnlyValue({
  value,
  mono = false,
}: {
  value: string;
  mono?: boolean;
}): ReactNode {
  return (
    <span
      className={[
        'text-text-muted selectable max-w-[24rem] truncate text-right text-xs',
        mono ? 'font-mono' : '',
      ].join(' ')}
      title={value}
    >
      {value}
    </span>
  );
}

/** Generates a stable id for pairing a label with a control. */
export function useControlId(prefix: string): string {
  const id = useId();
  return `${prefix}-${id}`;
}
