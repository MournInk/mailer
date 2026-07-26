/**
 * AI filter configuration. Any OpenAI-compatible chat endpoint works, so the
 * form is base URL + key + model rather than a provider list; the presets are
 * only shortcuts for the three endpoints people actually use.
 *
 * The key is write-only: `AiSettingsPublic` reports whether one is stored
 * (`hasApiKey`) but never its value, and an empty field keeps it.
 */

import { useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import type { AiSettingsPublic, TestResult } from "../../lib/types";
import { Icon } from "../Icon";
import { Group, Section, SwitchField, TestOutput } from "./parts";

/** Endpoint shortcuts: base URL + a model that is cheap and fast enough. */
const ENDPOINT_PRESETS: Array<{ name: string; apiBase: string; model: string }> = [
  { name: "OpenAI", apiBase: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  { name: "DeepSeek", apiBase: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  { name: "Ollama 本地", apiBase: "http://127.0.0.1:11434/v1", model: "qwen2.5:7b" },
];

interface Draft {
  enabled: boolean;
  apiBase: string;
  apiKey: string;
  model: string;
  temperature: number;
  autoDeleteSpam: boolean;
  extraInstructions: string;
}

function draftFrom(s: AiSettingsPublic): Draft {
  return {
    enabled: s.enabled,
    apiBase: s.apiBase,
    apiKey: "",
    model: s.model,
    temperature: s.temperature,
    autoDeleteSpam: s.autoDeleteSpam,
    extraInstructions: s.extraInstructions,
  };
}

export function AiTab() {
  const { pushToast } = useApp();

  const [stored, setStored] = useState<AiSettingsPublic | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [result, setResult] = useState<TestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let alive = true;
    void api
      .getAiSettings()
      .then((s) => {
        if (!alive) return;
        setStored(s);
        setDraft(draftFrom(s));
      })
      .catch((e) => alive && setLoadError(String(e)));
    return () => {
      alive = false;
    };
  }, []);

  if (loadError) {
    return (
      <Section title="AI 过滤器" icon="spark">
        <p className="set-error" role="alert">
          <Icon name="alert" size={14} />
          <span>读取设置失败：{loadError}</span>
        </p>
      </Section>
    );
  }
  if (!draft || !stored) {
    return (
      <Section title="AI 过滤器" icon="spark">
        <p className="set-loading">
          <Icon name="loader" size={15} className="set-spin" />
          正在读取设置…
        </p>
      </Section>
    );
  }

  const patch = (p: Partial<Draft>) => setDraft((d) => (d ? { ...d, ...p } : d));
  const busy = saving || testing;

  const save = async () => {
    setSaving(true);
    try {
      const next = await api.setAiSettings({
        enabled: draft.enabled,
        apiBase: draft.apiBase.trim(),
        // empty → the backend keeps the stored key
        apiKey: draft.apiKey,
        model: draft.model.trim(),
        temperature: draft.temperature,
        autoDeleteSpam: draft.autoDeleteSpam,
        extraInstructions: draft.extraInstructions,
      });
      setStored(next);
      // never keep a secret in component state longer than the request needs
      setDraft(draftFrom(next));
      pushToast("ok", "AI 设置已保存");
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const test = async () => {
    setTesting(true);
    setResult(null);
    try {
      setResult(await api.testAi());
    } catch (e) {
      setResult({ ok: false, message: String(e) });
    } finally {
      setTesting(false);
    }
  };

  return (
    <>
      <Section
        title="AI 过滤器"
        icon="spark"
        sub="每封新邮件会发送标题、发件人与正文摘要给模型，用于判断类别、提取验证码并生成摘要。"
      >
        <SwitchField
          label="启用 AI 分类"
          hint="关闭后邮件仍会正常收取，但不会自动分类，也不会触发通知渠道。"
          checked={draft.enabled}
          onChange={(enabled) => patch({ enabled })}
          disabled={busy}
        />
      </Section>

      <Section
        title="模型接口"
        icon="bot"
        sub="兼容 OpenAI Chat Completions 协议的任意服务。"
      >
        <div className="field">
          <span className="field-label">快速填充</span>
          <div className="set-choices">
            {ENDPOINT_PRESETS.map((p) => (
              <button
                key={p.name}
                type="button"
                className={`set-choice${
                  draft.apiBase.trim().replace(/\/+$/, "") === p.apiBase ? " active" : ""
                }`}
                disabled={busy}
                onClick={() => patch({ apiBase: p.apiBase, model: p.model })}
              >
                <span className="set-choice-icon">
                  <Icon name="link" size={15} />
                </span>
                <span className="set-choice-label">{p.name}</span>
              </button>
            ))}
          </div>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="ai-base">
            接口地址
          </label>
          <input
            id="ai-base"
            className="input set-mono"
            value={draft.apiBase}
            disabled={busy}
            autoComplete="off"
            spellCheck={false}
            placeholder="https://api.openai.com/v1"
            onChange={(e) => patch({ apiBase: e.target.value })}
          />
          <p className="field-hint">
            请求会从本机直接发往该地址，不经过任何中转服务器；API Key 保存在本机数据库，当前尚未加密存储。
          </p>
        </div>

        <div className="set-grid">
          <div className="field">
            <label className="field-label" htmlFor="ai-key">
              API Key
            </label>
            <input
              id="ai-key"
              className="input"
              type="password"
              value={draft.apiKey}
              disabled={busy}
              autoComplete="new-password"
              placeholder={stored.hasApiKey ? "保持不变" : "sk-…"}
              onChange={(e) => patch({ apiKey: e.target.value })}
            />
            <p className="field-hint">
              {stored.hasApiKey
                ? "已配置密钥，留空则保持不变。"
                : "尚未配置密钥。本地模型（如 Ollama）可随意填写一个占位值。"}
            </p>
          </div>
          <div className="field">
            <label className="field-label" htmlFor="ai-model">
              模型
            </label>
            <input
              id="ai-model"
              className="input set-mono"
              value={draft.model}
              disabled={busy}
              autoComplete="off"
              spellCheck={false}
              placeholder="gpt-4o-mini"
              onChange={(e) => patch({ model: e.target.value })}
            />
            <p className="field-hint">分类任务很轻，小模型通常已经够用。</p>
          </div>
        </div>

        <Group title="生成参数" hint="分类需要稳定输出，温度越低结果越一致。">
          <div className="field">
            <label className="field-label" htmlFor="ai-temp">
              温度 <span className="set-range-value">{draft.temperature.toFixed(2)}</span>
            </label>
            <input
              id="ai-temp"
              className="set-range"
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={draft.temperature}
              disabled={busy}
              onChange={(e) => patch({ temperature: Number(e.target.value) })}
            />
            <p className="field-hint">分类需要稳定输出，建议保持在 0.2 以下。</p>
          </div>
        </Group>
      </Section>

      <Section
        title="分类偏好"
        icon="filter"
        sub="用一句话描述你的判断标准，模型会在分类时一并参考。"
      >
        <div className="field">
          <label className="field-label" htmlFor="ai-extra">
            补充规则
          </label>
          <textarea
            id="ai-extra"
            className="textarea"
            value={draft.extraInstructions}
            disabled={busy}
            placeholder="例如：来自 GitHub 的构建失败通知算重要；招聘邮件一律算垃圾邮件。"
            onChange={(e) => patch({ extraInstructions: e.target.value })}
          />
        </div>

        <div className={`set-warn${draft.autoDeleteSpam ? " armed" : ""}`}>
          <div className="set-warn-head">
            <span className="set-warn-mark">
              <Icon name="trash" size={14} />
            </span>
            <span className="set-warn-title">自动删除垃圾邮件</span>
            <button
              type="button"
              className={`switch${draft.autoDeleteSpam ? " on" : ""}`}
              role="switch"
              aria-checked={draft.autoDeleteSpam}
              aria-label="自动删除垃圾邮件"
              disabled={busy}
              onClick={() => patch({ autoDeleteSpam: !draft.autoDeleteSpam })}
            />
          </div>
          <p className="set-warn-body">
            开启后，只有被模型明确判定为「毫无价值」的垃圾邮件会被删除，
            并且会<strong>同时从服务器删除</strong>，无法恢复。
            验证码、账单、通知等一律不会被删除。若不确定，建议先保持关闭，
            观察一段时间的分类结果再决定。
          </p>
        </div>
      </Section>

      <TestOutput result={result} />

      <div className="set-actions">
        <p className="field-hint set-actions-hint">
          测试连接使用已保存的配置，修改后请先保存。
        </p>
        <button className="btn" onClick={() => void test()} disabled={busy}>
          <Icon
            name={testing ? "loader" : "bot"}
            size={15}
            className={testing ? "set-spin" : undefined}
          />
          {testing ? "测试中…" : "测试连接"}
        </button>
        <button className="btn btn-primary" onClick={() => void save()} disabled={busy}>
          <Icon
            name={saving ? "loader" : "check"}
            size={15}
            className={saving ? "set-spin" : undefined}
          />
          {saving ? "保存中…" : "保存"}
        </button>
      </div>
    </>
  );
}
