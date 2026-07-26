/**
 * Retrieval configuration: the embedding model that builds the index, the
 * reranker that reorders what it finds, and the memory the assistant keeps.
 *
 * Without an embedding model configured, retrieval silently degrades to
 * substring matching — the assistant still answers, but from whatever the
 * keyword search happened to catch, which is what makes it feel like it is
 * guessing. This screen is how that gets turned on, so the index counter is
 * shown prominently rather than buried.
 */

import { useCallback, useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import type {
  AiProvider,
  EmbeddingSettingsPublic,
  IndexStatus,
  MemoryEntry,
  MemoryKind,
  RerankerKind,
  RerankerSettingsPublic,
  TestResult,
} from "../../lib/types";
import { Icon } from "../Icon";
import { Group, Section, SwitchField, TestOutput } from "./parts";

/** Providers that actually expose an embeddings endpoint. */
const EMBED_PROVIDERS: Array<{ id: AiProvider; name: string }> = [
  { id: "openai-compatible", name: "OpenAI 兼容" },
  { id: "openai-responses", name: "OpenAI" },
  { id: "gemini", name: "Google Gemini" },
];

const EMBED_PRESETS: Array<{
  name: string;
  provider: AiProvider;
  apiBase: string;
  model: string;
  dimensions: number;
}> = [
  { name: "OpenAI small", provider: "openai-compatible", apiBase: "https://api.openai.com/v1", model: "text-embedding-3-small", dimensions: 1536 },
  { name: "硅基流动 BGE", provider: "openai-compatible", apiBase: "https://api.siliconflow.cn/v1", model: "BAAI/bge-m3", dimensions: 1024 },
  { name: "Ollama 本地", provider: "openai-compatible", apiBase: "http://127.0.0.1:11434/v1", model: "bge-m3", dimensions: 1024 },
  { name: "Gemini", provider: "gemini", apiBase: "https://generativelanguage.googleapis.com/v1beta", model: "text-embedding-004", dimensions: 768 },
];

const RERANKERS: Array<{ id: RerankerKind; name: string; hint: string }> = [
  { id: "none", name: "不重排", hint: "直接使用向量相似度的顺序，最快" },
  { id: "rerank-api", name: "重排接口", hint: "调用 /rerank，兼容 Jina、Cohere、Xinference、TEI" },
  { id: "llm-scoring", name: "模型打分", hint: "让对话模型逐条打分，无需额外服务，但每次多一轮请求" },
];

const MEMORY_KINDS: Array<{ id: MemoryKind; label: string }> = [
  { id: "preference", label: "偏好" },
  { id: "fact", label: "事实" },
  { id: "contact", label: "联系人" },
];

export function KnowledgeTab() {
  const { pushToast } = useApp();

  const [embed, setEmbed] = useState<EmbeddingSettingsPublic | null>(null);
  const [embedKey, setEmbedKey] = useState("");
  const [rerank, setRerank] = useState<RerankerSettingsPublic | null>(null);
  const [rerankKey, setRerankKey] = useState("");
  const [index, setIndex] = useState<IndexStatus | null>(null);
  const [memories, setMemories] = useState<MemoryEntry[]>([]);
  const [newMemory, setNewMemory] = useState("");
  const [newKind, setNewKind] = useState<MemoryKind>("preference");
  const [busy, setBusy] = useState(false);
  const [probe, setProbe] = useState<TestResult | null>(null);

  const load = useCallback(async () => {
    try {
      const [e, r, i, m] = await Promise.all([
        api.getEmbeddingSettings(),
        api.getRerankerSettings(),
        api.indexStatus(),
        api.listMemories(),
      ]);
      setEmbed(e);
      setRerank(r);
      setIndex(i);
      setMemories(m);
    } catch (err) {
      pushToast("error", `读取设置失败: ${err}`);
    }
  }, [pushToast]);

  useEffect(() => {
    void load();
  }, [load]);

  // The backfill runs in the background and pushes progress, so the counter
  // moves without the user reopening the screen.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void api
      .onIndexStatus((s) => setIndex(s))
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  if (!embed || !rerank) {
    return (
      <Section title="知识库" icon="archive">
        <p className="set-loading">
          <Icon name="loader" size={15} className="set-spin" />
          正在读取设置…
        </p>
      </Section>
    );
  }

  const patchEmbed = (p: Partial<EmbeddingSettingsPublic>) =>
    setEmbed((s) => (s ? { ...s, ...p } : s));
  const patchRerank = (p: Partial<RerankerSettingsPublic>) =>
    setRerank((s) => (s ? { ...s, ...p } : s));

  const saveEmbed = async () => {
    setBusy(true);
    try {
      const next = await api.setEmbeddingSettings({
        enabled: embed.enabled,
        provider: embed.provider,
        apiBase: embed.apiBase.trim(),
        apiKey: embedKey,
        model: embed.model.trim(),
        dimensions: embed.dimensions,
      });
      setEmbed(next);
      setEmbedKey("");
      setIndex(await api.indexStatus());
      pushToast("ok", "向量模型设置已保存");
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const saveRerank = async () => {
    setBusy(true);
    try {
      const next = await api.setRerankerSettings({
        kind: rerank.kind,
        apiBase: rerank.apiBase.trim(),
        apiKey: rerankKey,
        model: rerank.model.trim(),
        candidates: rerank.candidates,
        topN: rerank.topN,
      });
      setRerank(next);
      setRerankKey("");
      pushToast("ok", "重排设置已保存");
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const pct =
    index && index.total > 0 ? Math.round((index.indexed / index.total) * 100) : 0;
  // The deep index is a second pass over starred mail only, so it has its own
  // denominator — showing it against the whole mailbox would read as stalled.
  const deepPct =
    index && index.deepTotal > 0
      ? Math.round((index.deepIndexed / index.deepTotal) * 100)
      : 0;

  return (
    <>
      <Section
        title="向量模型"
        icon="archive"
        sub="把邮件转成向量后存进本机索引，助手才能按语义检索而不是靠关键词。"
      >
        <SwitchField
          label="启用语义检索"
          hint="关闭后助手仍可使用，但只能按关键词匹配邮件。"
          checked={embed.enabled}
          onChange={(enabled) => patchEmbed({ enabled })}
          disabled={busy}
        />

        <div className="field">
          <span className="field-label">快速填充</span>
          <div className="set-choices">
            {EMBED_PRESETS.map((p) => (
              <button
                key={p.name}
                type="button"
                className={`set-choice${
                  embed.model === p.model && embed.provider === p.provider ? " active" : ""
                }`}
                disabled={busy}
                onClick={() =>
                  patchEmbed({
                    provider: p.provider,
                    apiBase: p.apiBase,
                    model: p.model,
                    dimensions: p.dimensions,
                  })
                }
              >
                <span className="set-choice-icon">
                  <Icon name="link" size={15} />
                </span>
                <span className="set-choice-label">{p.name}</span>
              </button>
            ))}
          </div>
        </div>

        <Group title="接口">
          <div className="field">
            <span className="field-label">接口协议</span>
            <div className="set-choices">
              {EMBED_PROVIDERS.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className={`set-choice${embed.provider === p.id ? " active" : ""}`}
                  disabled={busy}
                  onClick={() => patchEmbed({ provider: p.id })}
                >
                  <span className="set-choice-label">{p.name}</span>
                </button>
              ))}
            </div>
            <p className="field-hint">
              Anthropic 没有 embedding 接口，所以不在此列；对话模型仍可单独使用它。
            </p>
          </div>

          <label className="field">
            <span className="field-label">接口地址</span>
            <input
              className="input set-mono"
              value={embed.apiBase}
              disabled={busy}
              spellCheck={false}
              onChange={(e) => patchEmbed({ apiBase: e.target.value })}
            />
          </label>

          <label className="field">
            <span className="field-label">
              API Key{embed.hasApiKey ? "（已保存，留空表示不修改）" : ""}
            </span>
            <input
              className="input set-mono"
              type="password"
              value={embedKey}
              placeholder={embed.hasApiKey ? "••••••••" : "sk-…"}
              disabled={busy}
              autoComplete="off"
              onChange={(e) => setEmbedKey(e.target.value)}
            />
          </label>

          <div className="set-row2">
            <label className="field">
              <span className="field-label">模型</span>
              <input
                className="input set-mono"
                value={embed.model}
                disabled={busy}
                spellCheck={false}
                onChange={(e) => patchEmbed({ model: e.target.value })}
              />
            </label>
            <label className="field">
              <span className="field-label">维度（0 表示由模型决定）</span>
              <input
                className="input"
                type="number"
                min={0}
                value={embed.dimensions}
                disabled={busy}
                onChange={(e) => patchEmbed({ dimensions: Number(e.target.value) || 0 })}
              />
            </label>
          </div>
        </Group>

        <div className="set-actions">
          <button className="btn btn-primary" onClick={() => void saveEmbed()} disabled={busy}>
            保存
          </button>
          <button
            className="btn"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                setProbe(await api.testEmbedding());
              } catch (e) {
                setProbe({ ok: false, message: String(e) });
              } finally {
                setBusy(false);
              }
            }}
          >
            测试连接
          </button>
        </div>
        {probe && <TestOutput result={probe} />}
      </Section>

      <Section title="索引进度" icon="loader" sub="只有已建立索引的邮件才能被语义检索到。">
        <div className="kb-index">
          <div className="kb-bar" aria-hidden>
            <span className="kb-bar-fill" style={{ width: `${pct}%` }} />
          </div>
          <p className="kb-index-text">
            已索引 <strong>{index?.indexed ?? 0}</strong> / {index?.total ?? 0} 封
            {index?.model ? ` · 模型 ${index.model}` : ""}
            {index?.building ? " · 正在建立…" : ""}
          </p>
          {/* Starred mail gets a second, finer pass: the whole body in chunks
              instead of one vector per message. Hidden until something is
              starred, because a 0/0 bar looks broken. */}
          {index != null && index.deepTotal > 0 && (
            <>
              <div className="kb-bar" aria-hidden>
                <span className="kb-bar-fill deep" style={{ width: `${deepPct}%` }} />
              </div>
              <p className="kb-index-text">
                收藏邮件全文精读 <strong>{index.deepIndexed}</strong> / {index.deepTotal} 封
                <span className="field-hint kb-deep-hint">
                  收藏的邮件会按段落整篇建立索引，长邮件里的细节也能被问到。
                </span>
              </p>
            </>
          )}
          {index?.error && (
            <p className="kb-index-error">
              <Icon name="alert" size={13} /> {index.error}
            </p>
          )}
        </div>
        <div className="set-actions">
          <button
            className="btn btn-primary"
            disabled={busy || !embed.enabled || index?.building}
            onClick={async () => {
              try {
                setIndex(await api.indexPending());
                pushToast("info", "已开始建立索引，可离开此页面");
              } catch (e) {
                pushToast("error", `启动失败: ${e}`);
              }
            }}
          >
            <Icon name="refresh" size={15} />
            建立 / 补全索引
          </button>
          <button
            className="btn btn-danger"
            disabled={busy}
            onClick={async () => {
              try {
                setIndex(await api.clearIndex());
                pushToast("ok", "索引已清空");
              } catch (e) {
                pushToast("error", `清空失败: ${e}`);
              }
            }}
          >
            清空索引
          </button>
        </div>
      </Section>

      <Section
        title="重排模型"
        icon="filter"
        sub="向量检索给出候选后，再排一次序，让最相关的排在前面。"
      >
        <div className="field">
          <span className="field-label">重排方式</span>
          <div className="set-choices">
            {RERANKERS.map((r) => (
              <button
                key={r.id}
                type="button"
                className={`set-choice${rerank.kind === r.id ? " active" : ""}`}
                disabled={busy}
                title={r.hint}
                onClick={() => patchRerank({ kind: r.id })}
              >
                <span className="set-choice-label">{r.name}</span>
              </button>
            ))}
          </div>
          <p className="field-hint">{RERANKERS.find((r) => r.id === rerank.kind)?.hint}</p>
        </div>

        {rerank.kind === "rerank-api" && (
          <Group title="重排接口">
            <label className="field">
              <span className="field-label">接口地址</span>
              <input
                className="input set-mono"
                value={rerank.apiBase}
                disabled={busy}
                spellCheck={false}
                onChange={(e) => patchRerank({ apiBase: e.target.value })}
              />
            </label>
            <label className="field">
              <span className="field-label">
                API Key{rerank.hasApiKey ? "（已保存，留空表示不修改）" : ""}
              </span>
              <input
                className="input set-mono"
                type="password"
                value={rerankKey}
                placeholder={rerank.hasApiKey ? "••••••••" : ""}
                disabled={busy}
                autoComplete="off"
                onChange={(e) => setRerankKey(e.target.value)}
              />
            </label>
            <label className="field">
              <span className="field-label">模型</span>
              <input
                className="input set-mono"
                value={rerank.model}
                disabled={busy}
                spellCheck={false}
                onChange={(e) => patchRerank({ model: e.target.value })}
              />
            </label>
          </Group>
        )}

        <div className="set-row2">
          <label className="field">
            <span className="field-label">候选数量</span>
            <input
              className="input"
              type="number"
              min={1}
              value={rerank.candidates}
              disabled={busy}
              onChange={(e) => patchRerank({ candidates: Number(e.target.value) || 1 })}
            />
          </label>
          <label className="field">
            <span className="field-label">最终保留</span>
            <input
              className="input"
              type="number"
              min={1}
              value={rerank.topN}
              disabled={busy}
              onChange={(e) => patchRerank({ topN: Number(e.target.value) || 1 })}
            />
          </label>
        </div>

        <div className="set-actions">
          <button className="btn btn-primary" onClick={() => void saveRerank()} disabled={busy}>
            保存
          </button>
        </div>
      </Section>

      <Section
        title="助手记忆"
        icon="bot"
        sub="助手会记住这些偏好与事实，在回答时一并参考。"
      >
        <div className="kb-mem-add">
          <select
            className="select kb-mem-kind"
            value={newKind}
            onChange={(e) => setNewKind(e.target.value as MemoryKind)}
          >
            {MEMORY_KINDS.map((k) => (
              <option key={k.id} value={k.id}>
                {k.label}
              </option>
            ))}
          </select>
          <input
            className="input"
            value={newMemory}
            placeholder="例如：回复邮件时语气正式一些"
            onChange={(e) => setNewMemory(e.target.value)}
          />
          <button
            className="btn btn-primary"
            disabled={!newMemory.trim()}
            onClick={async () => {
              try {
                await api.saveMemory({ kind: newKind, text: newMemory.trim() });
                setNewMemory("");
                setMemories(await api.listMemories());
              } catch (e) {
                pushToast("error", `保存失败: ${e}`);
              }
            }}
          >
            添加
          </button>
        </div>

        {memories.length === 0 ? (
          <p className="field-hint">还没有记忆条目。你可以手动添加，助手也会在对话中自行记录。</p>
        ) : (
          <ul className="kb-mem-list">
            {memories.map((m) => (
              <li key={m.id} className="kb-mem">
                <span className={`badge badge-${m.kind === "contact" ? "verification" : m.kind === "fact" ? "normal" : "important"}`}>
                  {MEMORY_KINDS.find((k) => k.id === m.kind)?.label}
                </span>
                <span className="kb-mem-text">{m.text}</span>
                <button
                  className="icon-btn"
                  aria-label="删除这条记忆"
                  onClick={async () => {
                    try {
                      await api.deleteMemory(m.id);
                      setMemories(await api.listMemories());
                    } catch (e) {
                      pushToast("error", `删除失败: ${e}`);
                    }
                  }}
                >
                  <Icon name="trash" size={14} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}
