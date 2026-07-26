/**
 * Small building blocks shared by the four settings tabs: the section card,
 * the label + switch row, and the mono block that renders a `TestResult`.
 */

import type { ReactNode } from "react";
import { Icon } from "../Icon";
import type { TestResult } from "../../lib/types";

/** A titled card. `action` sits on the title row, right-aligned. */
export function Section({
  title,
  sub,
  action,
  children,
}: {
  title: string;
  sub?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="card set-section">
      <header className="set-section-head">
        <div className="set-section-text">
          <h2 className="set-section-title">{title}</h2>
          {sub && <p className="set-section-sub">{sub}</p>}
        </div>
        {action}
      </header>
      <div className="set-section-body">{children}</div>
    </section>
  );
}

/**
 * Label + description on the left, switch on the right. The whole row is the
 * hit target — a 38px switch is a poor one on a phone.
 */
export function SwitchField({
  label,
  hint,
  checked,
  onChange,
  disabled = false,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className="set-switch-row"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="set-switch-text">
        <span className="set-switch-label">{label}</span>
        {hint && <span className="field-hint">{hint}</span>}
      </span>
      <span className={`switch${checked ? " on" : ""}`} />
    </button>
  );
}

/**
 * Connection-test output. The backend composes multi-line, already-localized
 * text (one line per checked service) — render it verbatim, never reworded.
 */
export function TestOutput({ result }: { result: TestResult | null }) {
  if (!result) return null;
  return (
    <div className={`set-test${result.ok ? " ok" : " bad"}`} role="status">
      <Icon name={result.ok ? "check" : "alert"} size={15} />
      <pre className="set-test-text">{result.message}</pre>
    </div>
  );
}
