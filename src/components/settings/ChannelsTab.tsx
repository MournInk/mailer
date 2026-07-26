/**
 * Notification channels: the list plus the kind-driven editor.
 *
 * The enabled switch saves on the spot — muting a channel is something people
 * do in a hurry, and it should not need a trip through the editor. Everything
 * else goes through 编辑.
 */

import { useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import { CATEGORY_LABEL, type NotifyChannel, type TestResult } from "../../lib/types";
import { Icon } from "../Icon";
import { ChannelForm } from "./ChannelForm";
import { KIND_META } from "./channelKinds";
import { TestOutput } from "./parts";

/** null = list only; { channel: null } = add form; { channel } = edit form. */
type Editing = { channel: NotifyChannel | null } | null;

export function ChannelsTab() {
  const { pushToast } = useApp();

  const [channels, setChannels] = useState<NotifyChannel[]>([]);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState<Editing>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [tested, setTested] = useState<{ id: string; result: TestResult } | null>(null);

  const reload = async () => {
    try {
      setChannels(await api.listChannels());
    } catch (e) {
      pushToast("error", `读取通知渠道失败: ${e}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const submit = async (channel: NotifyChannel) => {
    setSaving(true);
    try {
      await api.saveChannel(channel);
      await reload();
      setEditing(null);
      pushToast("ok", channel.id ? "渠道已更新" : "渠道已添加");
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (channel: NotifyChannel) => {
    setBusyId(channel.id);
    try {
      await api.saveChannel({ ...channel, enabled: !channel.enabled });
      await reload();
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setBusyId(null);
    }
  };

  const test = async (channel: NotifyChannel) => {
    setBusyId(channel.id);
    setTested(null);
    try {
      setTested({ id: channel.id, result: await api.testChannel(channel.id) });
    } catch (e) {
      setTested({ id: channel.id, result: { ok: false, message: String(e) } });
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (channel: NotifyChannel) => {
    setBusyId(channel.id);
    try {
      await api.deleteChannel(channel.id);
      setConfirmId(null);
      await reload();
      pushToast("ok", `已删除「${channel.name}」`);
    } catch (e) {
      pushToast("error", `删除失败: ${e}`);
    } finally {
      setBusyId(null);
    }
  };

  if (editing) {
    return (
      <ChannelForm
        key={editing.channel?.id ?? "new"}
        channel={editing.channel}
        saving={saving}
        onSubmit={(c) => void submit(c)}
        onCancel={() => setEditing(null)}
      />
    );
  }

  return (
    <>
      <div className="set-toolbar">
        <p className="set-toolbar-text">
          把重要邮件、验证码实时转发到手机，不用一直盯着收件箱。
        </p>
        <button
          className="btn btn-primary"
          onClick={() => {
            setConfirmId(null);
            setEditing({ channel: null });
          }}
        >
          <Icon name="plus" size={16} />
          添加渠道
        </button>
      </div>

      {loading && (
        <p className="set-loading">
          <Icon name="loader" size={15} className="set-spin" />
          正在读取通知渠道…
        </p>
      )}

      {!loading && channels.length === 0 && (
        <div className="card set-empty">
          <span className="set-empty-mark">
            <Icon name="bell" size={18} />
          </span>
          <span className="set-empty-title">还没有通知渠道</span>
          <p className="set-empty-body">
            添加 Telegram、QQ 机器人、Bark 或自定义 Webhook，
            AI 判定为重要的邮件会立刻推送过去。
          </p>
        </div>
      )}

      <div className="set-cards">
        {channels.map((c) => {
          const meta = KIND_META[c.kind];
          const busy = busyId === c.id;
          return (
            <article key={c.id} className="card set-card">
              <div className="set-card-main">
                <span className={`set-kind-mark${c.enabled ? " on" : ""}`}>
                  <Icon name={meta.icon} size={17} />
                </span>
                <div className="set-card-text">
                  <div className="set-card-title">
                    {c.name}
                    {!c.enabled && <span className="set-muted-tag">已停用</span>}
                  </div>
                  <div className="set-card-mail">{meta.label}</div>
                  <div className="set-card-cats">
                    {c.notifyCategories.length === 0 ? (
                      <span className="field-hint">未选择类别</span>
                    ) : (
                      c.notifyCategories.map((cat) => (
                        <span key={cat} className={`badge badge-${cat}`}>
                          {CATEGORY_LABEL[cat]}
                        </span>
                      ))
                    )}
                  </div>
                </div>
                <button
                  type="button"
                  className={`switch${c.enabled ? " on" : ""}`}
                  role="switch"
                  aria-checked={c.enabled}
                  aria-label={`启用「${c.name}」`}
                  title={c.enabled ? "点击停用" : "点击启用"}
                  disabled={busy}
                  onClick={() => void toggle(c)}
                />
              </div>

              <div className="set-card-foot">
                <button
                  className="btn btn-sm"
                  disabled={busy}
                  onClick={() => void test(c)}
                >
                  <Icon
                    name={busy ? "loader" : "send"}
                    size={14}
                    className={busy ? "set-spin" : undefined}
                  />
                  测试
                </button>
                <button
                  className="btn btn-sm"
                  disabled={busy}
                  onClick={() => {
                    setConfirmId(null);
                    setEditing({ channel: c });
                  }}
                >
                  <Icon name="edit" size={14} />
                  编辑
                </button>
                <button
                  className="btn btn-sm btn-danger"
                  disabled={busy}
                  onClick={() => setConfirmId(confirmId === c.id ? null : c.id)}
                >
                  <Icon name="trash" size={14} />
                  删除
                </button>
              </div>

              {tested?.id === c.id && <TestOutput result={tested.result} />}

              {confirmId === c.id && (
                <div className="set-confirm" role="alertdialog">
                  <Icon name="alert" size={15} />
                  <span className="set-confirm-text">
                    删除「{c.name}」后将不再向该渠道推送任何邮件。
                  </span>
                  <div className="set-confirm-actions">
                    <button
                      className="btn btn-sm"
                      disabled={busy}
                      onClick={() => setConfirmId(null)}
                    >
                      取消
                    </button>
                    <button
                      className="btn btn-sm btn-danger"
                      disabled={busy}
                      onClick={() => void remove(c)}
                    >
                      确认删除
                    </button>
                  </div>
                </div>
              )}
            </article>
          );
        })}
      </div>
    </>
  );
}
