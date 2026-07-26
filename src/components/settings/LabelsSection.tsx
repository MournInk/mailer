/**
 * The user's own categories, defined in their own words.
 *
 * The four built-in categories say what a mail *is* — a code, a bill, a blast.
 * They cannot say what it is about *to this person*: 候选人简历, 房东通知,
 * 报销单据. Those differ for everyone, which is why the definition here is a
 * sentence rather than a rule builder: "求职者投递简历或跟进面试进度" is not
 * expressible as a filter, and it is exactly what the triage model is good at.
 *
 * The sentence is the feature. The form treats it that way — it is the field
 * with the real validation, and the placeholder shows what a good one looks
 * like.
 */

import { useCallback, useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import type { MailLabel } from "../../lib/types";
import { Icon } from "../Icon";
import { Section } from "./parts";

/** Hues offered for the sidebar dot. Same set the accounts use. */
const HUES = [8, 32, 88, 152, 186, 210, 258, 300];

const EMPTY = { id: "", name: "", instruction: "", colorHue: 210, enabled: true };

export function LabelsSection() {
  const { pushToast, refreshLabels } = useApp();
  const [labels, setLabels] = useState<MailLabel[]>([]);
  const [draft, setDraft] = useState<typeof EMPTY | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLabels(await api.listLabels());
    } catch (e) {
      pushToast("error", `读取标签失败: ${e}`);
    }
  }, [pushToast]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    if (!draft) return;
    setBusy(true);
    try {
      setLabels(await api.saveLabel({ ...draft, id: draft.id || null }));
      setDraft(null);
      await refreshLabels();
      pushToast("ok", draft.id ? "标签已更新" : "标签已添加，新邮件到达时开始应用");
    } catch (e) {
      pushToast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (l: MailLabel) => {
    setBusy(true);
    try {
      setLabels(await api.saveLabel({ ...l, enabled: !l.enabled }));
      await refreshLabels();
    } catch (e) {
      pushToast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (l: MailLabel) => {
    setBusy(true);
    try {
      setLabels(await api.deleteLabel(l.id));
      setConfirmId(null);
      await refreshLabels();
      pushToast("ok", `已删除「${l.name}」`);
    } catch (e) {
      pushToast("error", `删除失败: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section
      title="我的标签"
      icon="tag"
      sub="除了内置的四类，你可以用一句话定义自己的分类，模型会在分类时一并判断。"
      action={
        !draft && (
          <button className="btn btn-sm" onClick={() => setDraft({ ...EMPTY })}>
            <Icon name="plus" size={14} />
            新建标签
          </button>
        )
      }
    >
      {labels.length === 0 && !draft && (
        <p className="field-hint">
          还没有标签。比如「候选人简历」——描述成「求职者投递简历或跟进面试进度」，
          之后这类邮件会自动归到侧栏的同名分组里。
        </p>
      )}

      {labels.length > 0 && (
        <ul className="lb-list">
          {labels.map((l) => (
            <li key={l.id} className="lb-row">
              <span
                className="lb-dot"
                style={{ background: `hsl(${l.colorHue} 62% 48%)` }}
                aria-hidden
              />
              <span className="lb-text">
                <span className="lb-name">
                  {l.name}
                  {!l.enabled && <span className="set-muted-tag">已停用</span>}
                </span>
                <span className="lb-instruction">{l.instruction}</span>
              </span>
              <button
                type="button"
                className={`switch${l.enabled ? " on" : ""}`}
                role="switch"
                aria-checked={l.enabled}
                aria-label={`启用「${l.name}」`}
                title={l.enabled ? "点击停用" : "点击启用"}
                disabled={busy}
                onClick={() => void toggle(l)}
              />
              <button
                className="icon-btn"
                aria-label="编辑"
                title="编辑"
                disabled={busy}
                onClick={() => {
                  setConfirmId(null);
                  setDraft({
                    id: l.id,
                    name: l.name,
                    instruction: l.instruction,
                    colorHue: l.colorHue,
                    enabled: l.enabled,
                  });
                }}
              >
                <Icon name="edit" size={14} />
              </button>
              <button
                className="icon-btn"
                aria-label="删除"
                title="删除"
                disabled={busy}
                onClick={() => setConfirmId(confirmId === l.id ? null : l.id)}
              >
                <Icon name="trash" size={14} />
              </button>

              {confirmId === l.id && (
                <div className="set-confirm" role="alertdialog">
                  <Icon name="alert" size={15} />
                  <span className="set-confirm-text">
                    删除「{l.name}」只会移除这个分组，邮件本身不受影响。
                  </span>
                  <button className="btn btn-sm" onClick={() => setConfirmId(null)}>
                    取消
                  </button>
                  <button
                    className="btn btn-sm btn-danger"
                    disabled={busy}
                    onClick={() => void remove(l)}
                  >
                    确认删除
                  </button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      {draft && (
        <div className="lb-form">
          <div className="field">
            <label className="field-label" htmlFor="lb-name">
              名称
            </label>
            <input
              id="lb-name"
              className="input"
              value={draft.name}
              disabled={busy}
              placeholder="候选人简历"
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            />
            <p className="field-hint">
              也是模型作答时要写出来的名字，取一个自己看得懂、含义明确的。
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="lb-inst">
              什么样的邮件属于它
            </label>
            <textarea
              id="lb-inst"
              className="textarea"
              value={draft.instruction}
              disabled={busy}
              placeholder="求职者投递简历，或跟进面试进度、询问结果的邮件。招聘平台的推广邮件不算。"
              onChange={(e) => setDraft({ ...draft, instruction: e.target.value })}
            />
            <p className="field-hint">
              用平常话写，把边界也写上——「什么不算」往往比「什么算」更有用。
            </p>
          </div>

          <div className="field">
            <span className="field-label">颜色</span>
            <div className="lb-hues">
              {HUES.map((h) => (
                <button
                  key={h}
                  type="button"
                  className={`lb-hue${draft.colorHue === h ? " active" : ""}`}
                  style={{ background: `hsl(${h} 62% 48%)` }}
                  aria-label={`色相 ${h}`}
                  aria-pressed={draft.colorHue === h}
                  disabled={busy}
                  onClick={() => setDraft({ ...draft, colorHue: h })}
                />
              ))}
            </div>
          </div>

          <div className="set-actions">
            <button className="btn" onClick={() => setDraft(null)} disabled={busy}>
              取消
            </button>
            <button className="btn btn-primary" onClick={() => void save()} disabled={busy}>
              <Icon name="check" size={15} />
              保存
            </button>
          </div>
        </div>
      )}

      <p className="set-note">
        <Icon name="spark" size={14} />
        <span>
          标签跟着分类一起判断，不额外花一次请求。已经分好类的旧邮件不会自动重新判断——
          在邮件里点「重新分类」可以让某一封重新过一遍。
        </span>
      </p>
    </Section>
  );
}
