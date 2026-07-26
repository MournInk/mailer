/**
 * Privacy: the tracker switch, and what it has been doing.
 *
 * The switch is the whole feature; the heatmap is the evidence. A blocker whose
 * work is invisible is indistinguishable from one that is switched off, and the
 * number it produces — "1 340 requests refused in ten weeks" — is the argument
 * for leaving it on.
 */

import { useCallback, useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import { TRACKER_KIND_LABEL, type TrackerStats } from "../../lib/types";
import { Icon } from "../Icon";
import { Section, SwitchField } from "./parts";

/** Cells per column. Weeks read down, like every contribution graph. */
const ROWS = 7;

export function PrivacyTab() {
  const { pushToast, blockTrackers, setBlockTrackers } = useApp();
  const [stats, setStats] = useState<TrackerStats | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setStats(await api.trackerStats());
    } catch (e) {
      pushToast("error", `读取拦截统计失败: ${e}`);
    }
  }, [pushToast]);

  useEffect(() => {
    void load();
  }, [load]);

  const days = stats?.days ?? [];
  // Intensity is relative to the busiest day in the window: an absolute scale
  // would leave a quiet mailbox looking empty and a loud one saturated.
  const peak = days.reduce((m, d) => Math.max(m, d.blocked), 0);
  const weeks: typeof days[] = [];
  for (let i = 0; i < days.length; i += ROWS) weeks.push(days.slice(i, i + ROWS));

  return (
    <>
      <Section
        title="阻止追踪器"
        icon="shield"
        sub="邮件里的远程图片多数不是图片，而是知道你何时打开了邮件的请求。"
      >
        <SwitchField
          label="阻止邮件中的追踪器与远程内容"
          hint="默认开启。打开某封邮件后仍可单独点「显示图片」，只对那一封生效。"
          checked={blockTrackers}
          onChange={(v) => {
            setBusy(true);
            void setBlockTrackers(v).finally(() => setBusy(false));
          }}
          disabled={busy}
        />
        <p className="set-note">
          <Icon name="shield" size={14} />
          <span>
            拦截在本机完成：被改写的地址不会被请求，也不会有任何东西离开这台机器。
            邮件到达时会记录它想加载什么，因此下面的统计覆盖收到的全部邮件，而不只是读过的。
          </span>
        </p>
      </Section>

      <Section
        title="拦截记录"
        icon="chart"
        sub="最近十周，按邮件到达的日期统计。颜色越深的那天，追踪请求越多。"
        action={
          <button className="btn btn-sm" onClick={() => void load()}>
            <Icon name="refresh" size={14} />
            刷新
          </button>
        }
      >
        {!stats ? (
          <p className="set-loading">
            <Icon name="loader" size={15} className="set-spin" />
            正在统计…
          </p>
        ) : (
          <>
            <div className="pv-totals">
              <div className="pv-total">
                <span className="pv-total-num">{stats.blocked}</span>
                <span className="pv-total-label">个追踪请求被拦截</span>
              </div>
              <div className="pv-total">
                <span className="pv-total-num">{stats.messages}</span>
                <span className="pv-total-label">封邮件带有追踪器</span>
              </div>
            </div>

            <div className="pv-heat" role="img" aria-label="最近十周的追踪器拦截热力图">
              {weeks.map((week, i) => (
                <div key={i} className="pv-heat-week">
                  {week.map((d) => (
                    <span
                      key={d.day}
                      className={`pv-cell pv-l${level(d.blocked, peak)}`}
                      title={`${d.day}：拦截 ${d.blocked} 个追踪请求（${d.messages} 封邮件）`}
                    />
                  ))}
                </div>
              ))}
            </div>
            <div className="pv-legend">
              <span className="pv-legend-label">少</span>
              {[0, 1, 2, 3, 4].map((l) => (
                <span key={l} className={`pv-cell pv-l${l}`} aria-hidden="true" />
              ))}
              <span className="pv-legend-label">多</span>
              {peak > 0 && (
                <span className="pv-legend-peak">单日最多 {peak} 个</span>
              )}
            </div>

            {stats.top.length > 0 && (
              <div className="pv-top">
                <div className="pv-top-head">拦截最多的来源</div>
                <ul className="pv-top-list">
                  {stats.top.map((t) => (
                    <li key={t.host} className="pv-top-row">
                      <span className={`mv-track-tag mv-track-${t.kind}`}>
                        {TRACKER_KIND_LABEL[t.kind]}
                      </span>
                      <span className="pv-top-host">{t.host}</span>
                      <span className="pv-top-bar" aria-hidden="true">
                        <span
                          className="pv-top-fill"
                          style={{ width: `${Math.max(4, (t.count / stats.top[0].count) * 100)}%` }}
                        />
                      </span>
                      <span className="pv-top-count">{t.count}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            {stats.blocked === 0 && (
              <p className="field-hint">
                最近十周还没有拦截到追踪器。新邮件到达后这里会自动更新。
              </p>
            )}
          </>
        )}
      </Section>
    </>
  );
}

/**
 * 0–4 for one day, relative to the busiest day shown.
 *
 * Anything non-zero is at least level 1: a day with one tracker on it is not the
 * same as a day with none, and that is the distinction the grid exists to make.
 */
function level(n: number, peak: number): number {
  if (n <= 0) return 0;
  if (peak <= 1) return 4;
  return Math.min(4, 1 + Math.floor((n / peak) * 3.999));
}
