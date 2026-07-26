/**
 * A scroll container whose scrollbar floats over the content instead of taking
 * a column away from it.
 *
 * Neither WebKit nor Chromium has a real overlay scrollbar on the desktop:
 * `::-webkit-scrollbar { width: 8px }` reserves those eight pixels of layout
 * width forever, whether or not the thumb is painted. In a 380px mail column
 * that gutter is visible as a permanent strip down the right edge and it pushes
 * every row off centre. So the native bar is hidden and this draws its own,
 * positioned over the content and faded out until the pointer is in the area or
 * the content is actually moving — which is what macOS and every touch platform
 * do by default.
 *
 * It stays a plain scroll container: no virtualisation, no scroll hijacking, no
 * transform. The wheel, the trackpad, `scrollIntoView`, keyboard paging and
 * anchoring all behave exactly as they did, because the element really is the
 * scroller. Only the bar is ours.
 */

import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type UIEvent,
} from "react";
import "./OverlayScroll.css";

/** How long the thumb lingers after the last scroll event. */
const FADE_AFTER = 900;
/** Shortest thumb we will draw, so a very long list still has a grab target. */
const MIN_THUMB = 28;

export interface OverlayScrollHandle {
  /** The scrolling element itself, for `scrollTop`, measurement and listeners. */
  readonly el: HTMLDivElement | null;
}

export function OverlayScroll({
  children,
  className,
  handle,
  onScroll,
  ...rest
}: {
  children: ReactNode;
  className?: string;
  /** Access to the underlying scroller — this is the element that scrolls. */
  handle?: React.Ref<OverlayScrollHandle>;
  onScroll?: (e: UIEvent<HTMLDivElement>) => void;
} & Omit<React.HTMLAttributes<HTMLDivElement>, "onScroll">) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [thumb, setThumb] = useState<{ top: number; height: number } | null>(null);
  const [active, setActive] = useState(false);
  const fadeTimer = useRef<number | undefined>(undefined);
  const drag = useRef<{ startY: number; startTop: number } | null>(null);

  useImperativeHandle(handle, () => ({ get el() { return ref.current; } }), []);

  const measure = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const { scrollHeight, clientHeight, scrollTop } = el;
    // Nothing to scroll: no bar at all, not a full-height thumb.
    if (scrollHeight - clientHeight < 1) {
      setThumb(null);
      return;
    }
    const height = Math.max(MIN_THUMB, (clientHeight / scrollHeight) * clientHeight);
    const travel = clientHeight - height;
    const progress = scrollTop / (scrollHeight - clientHeight);
    setThumb({ top: progress * travel, height });
  }, []);

  // Re-measure whenever the content or the viewport changes size. A mail list
  // grows by pages, and the thumb has to shrink with it.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    for (const child of Array.from(el.children)) observer.observe(child);
    return () => observer.disconnect();
  }, [measure, children]);

  useEffect(() => () => window.clearTimeout(fadeTimer.current), []);

  const show = useCallback(() => {
    setActive(true);
    window.clearTimeout(fadeTimer.current);
    fadeTimer.current = window.setTimeout(() => {
      // Keep it up while it is being dragged, however long that takes.
      if (!drag.current) setActive(false);
    }, FADE_AFTER);
  }, []);

  const handleScroll = useCallback(
    (e: UIEvent<HTMLDivElement>) => {
      measure();
      show();
      onScroll?.(e);
    },
    [measure, show, onScroll],
  );

  // Dragging the thumb, on the window so the pointer may leave the bar.
  const onThumbPointerDown = useCallback(
    (e: React.PointerEvent) => {
      const el = ref.current;
      if (!el || !thumb) return;
      e.preventDefault();
      drag.current = { startY: e.clientY, startTop: el.scrollTop };
      setActive(true);

      const onMove = (ev: PointerEvent) => {
        const scroller = ref.current;
        const start = drag.current;
        if (!scroller || !start) return;
        const travel = scroller.clientHeight - thumb.height;
        if (travel <= 0) return;
        const ratio = (scroller.scrollHeight - scroller.clientHeight) / travel;
        scroller.scrollTop = start.startTop + (ev.clientY - start.startY) * ratio;
      };
      const onUp = () => {
        drag.current = null;
        show();
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [thumb, show],
  );

  const style: CSSProperties | undefined = thumb
    ? { top: `${thumb.top}px`, height: `${thumb.height}px` }
    : undefined;

  return (
    <div className={`ovs${active ? " ovs-active" : ""}`}>
      <div
        {...rest}
        ref={ref}
        className={`ovs-view${className ? ` ${className}` : ""}`}
        onScroll={handleScroll}
        onPointerEnter={show}
        onPointerMove={show}
      >
        {children}
      </div>
      {thumb && (
        <div
          className="ovs-thumb"
          style={style}
          onPointerDown={onThumbPointerDown}
          aria-hidden
        />
      )}
    </div>
  );
}
