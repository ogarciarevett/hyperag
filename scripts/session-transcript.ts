import { createHash } from "node:crypto";
export type Source = "claude" | "codex";
export const hash = (s: string) => createHash("sha256").update(s).digest("hex");

// Conservative local redaction intentionally removes some non-secret opaque identifiers too.
export function redact(text: string) {
  // A structured env/argv dump can hide secrets under arbitrary field names: drop the whole input.
  if (/["']?(?:argv|args|arguments|process[._ ]args|process\.env|env|environment(?:[_ ]dump)?|command[_ ]line)["']?\s*[:=]/i.test(text) || /["'](?:[A-Z0-9_]+_(?:URL|KEY|TOKEN|SECRET|PASSWORD|DSN)|PATH|HOME|ENV|PWD)["']\s*:|^\s*(?:export\s+)?[A-Z_][A-Z0-9_]*=/m.test(text)) return "[REDACTED_PROCESS_OR_ENV_BLOCK]";
  const keys = String.raw`(?:phone|tel|mobile|password|secret|token|key|auth|credential|cookie|session)`;
  const credentials = String.raw`(?:authorization|set-cookie|api[_-]?key|access[_-]?key|passwd|(?:[a-z0-9]+[_-])*(?:password|secret|token|key|auth|credential|cookie|session)(?:[_-][a-z0-9]+)*)`;
  const value = String.raw`(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s"']+)`;
  if (new RegExp(String.raw`\b${keys}\b["']?\s*[:=]\s*[\[{]`, "i").test(text)) return "[REDACTED_CREDENTIAL_BLOCK]";
  return text
    .replace(/-----BEGIN [\w ]*PRIVATE KEY-----[\s\S]*?-----END [\w ]*PRIVATE KEY-----/g, "[REDACTED_PRIVATE_KEY]")
    .replace(/^.*(?:\b(?:argv|args|arguments|process[_ ]?args|command[_ ]?line|environment[_ ]?dump)\s*[:=]|^\s*(?:export\s+)?[A-Z_][A-Z0-9_]*=).*$/gm, "[REDACTED_PROCESS_OR_ENV]")
    .replace(/^\s*(?:[$>]\s*)?(?:\/[\w./-]+\/)?(?:env|printenv|ps|curl|ssh|docker|node|bun|python3?|codex|claude|hyperag|engram)\s+.*$/gm, "[REDACTED_PROCESS_OR_ENV]")
    .replace(/(\b(?:Authorization|Cookie|Set-Cookie)\s*:\s*)[^\r\n]+/gi, "$1<REDACTED>")
    .replace(new RegExp(String.raw`(--${credentials}(?:=|\s+))${value}`, "gi"), "$1<REDACTED>")
    .replace(new RegExp(String.raw`(\b(?:Bearer|Basic)\s+)${value}`, "gi"), "$1<REDACTED>")
    .replace(new RegExp(String.raw`(\b${keys}\b["']?\s*[:=]\s*)${value}`, "gi"), "$1<REDACTED>")
    .replace(new RegExp(String.raw`(\b${credentials}\b["']?\s*[:=]\s*)${value}`, "gi"), "$1<REDACTED>")
    .replace(/([a-z][a-z0-9+.-]*:\/\/)[^\s/@]+@/gi, "$1[REDACTED]@")
    .replace(/\b(?:AKIA|ASIA)[A-Z0-9]{16}\b|\beyJ[\w-]+\.[\w-]+\.[\w-]+\b/g, "[REDACTED_TOKEN]")
    .replace(/\b(?:sk-|ghp_|github_pat_|xox[baprs]-)[\w-]{10,}/g, "[REDACTED_TOKEN]")
    .replace(/\+\d[\d ().-]{7,}\d|(?:\(\d{3}\)\s*|\b\d{3}[ .-])\d{3}[ .-]\d{4}\b/g, "[REDACTED_PHONE]")
    .replace(/(\b(?:phone|telephone|tel|mobile|m[oó]vil|tel[eé]fono)\s*[:=]\s*["']?)\d[\d ().-]{5,}\d/gi, "$1[REDACTED_PHONE]")
    .replace(/\b(?:[679]\d{8}|0\d{9,10})\b/g, "[REDACTED_PHONE]")
    .replace(/[A-Za-z0-9_+/=~.-]{20,}/g, v => /[a-zA-Z]/.test(v) && /\d/.test(v) ? "[REDACTED_OPAQUE]" : v);
}

type Prompt = { text: string; timestamp: string | null; mirrorKey: string; order: number };
export function parseTranscript(source: Source, text: string) {
  const session = { id: null as string | null, cwd: null as string | null, startedAt: null as string | null, endedAt: null as string | null, firstEventAt: null as string | null, lastEventAt: null as string | null, model: null as string | null };
  let order = 0;
  const prompts: Prompt[] = [], fallback: Prompt[] = [], skipped: Record<string, number> = {};
  const skip = (why: string) => { skipped[why] = (skipped[why] ?? 0) + 1; };
  const string = (v: unknown) => typeof v === "string" ? v : null;
  const stamp = (v: unknown) => typeof v === "string" && Number.isFinite(Date.parse(v)) ? v : null;
  const add = (target: Prompt[], raw: unknown, timestamp: unknown) => {
    const content = typeof raw === "string" ? raw : Array.isArray(raw) ? raw.filter(b => b && ["text", "input_text"].includes(b.type) && typeof b.text === "string").map(b => b.text).join("\n") : "";
    if (!content.trim()) { skip("nonText"); return; }
    if (content.length > 16_000) { skip("largePrompt"); return; }
    if (/^\s*(?:# AGENTS\.md instructions|<environment_context>|<permissions instructions>|<local-command|<system-reminder>)/.test(content)) { skip("generatedContext"); return; }
    target.push({ text: redact(content), timestamp: stamp(timestamp), mirrorKey: hash(content), order: order++ });
  };
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    if (line.length > 256_000) { skip("largeRecord"); continue; }
    let r: any;
    try { r = JSON.parse(line); if (!r || typeof r !== "object") throw new Error(); } catch { skip("invalidJson"); continue; }
    const p = r.payload ?? {}, timestamp = stamp(r.timestamp);
    if (timestamp) {
      if (!session.firstEventAt || Date.parse(timestamp) < Date.parse(session.firstEventAt)) session.firstEventAt = timestamp;
      if (!session.lastEventAt || Date.parse(timestamp) > Date.parse(session.lastEventAt)) session.lastEventAt = timestamp;
    }
    if (["session_meta", "session_start"].includes(r.type)) {
      const start = stamp(p.started_at ?? p.timestamp ?? r.timestamp);
      if (start && (!session.startedAt || Date.parse(start) < Date.parse(session.startedAt))) session.startedAt = start;
    }
    const id = source === "codex" ? (r.type === "session_meta" ? string(p.id) : null) : string(r.sessionId);
    if (id && session.id && session.id !== id) throw new Error("mixed source session ids");
    session.id ??= id;
    session.cwd ??= string(source === "codex" ? p.cwd : r.cwd);
    if (source === "codex") {
      if (r.type === "turn_context") session.model = string(p.model) ?? session.model;
      if (r.type === "session_end") session.endedAt = timestamp;
      if (r.type === "response_item" && p.type === "message" && p.role === "user") add(prompts, p.content, r.timestamp);
      else if (r.type === "event_msg" && p.type === "user_message") add(fallback, p.message, r.timestamp);
      else skip("nonUserRecord");
    } else {
      session.model = string(r.message?.model) ?? session.model;
      if (r.type === "session_end") session.endedAt = timestamp;
      if (r.type === "user" && !r.isMeta) add(prompts, r.message?.content, r.timestamp);
      else skip("nonUserRecord");
    }
  }
  // Consume only corresponding event mirrors; unmatched tail events remain real user prompts.
  const mirrors = new Map<string, number>();
  for (const p of prompts) mirrors.set(p.mirrorKey, (mirrors.get(p.mirrorKey) ?? 0) + 1);
  const unmatched = fallback.filter(p => {
    const count = mirrors.get(p.mirrorKey) ?? 0;
    if (!count) return true;
    mirrors.set(p.mirrorKey, count - 1); skip("mirroredUserEvent"); return false;
  });
  const selected = [...prompts, ...unmatched].sort((a,b) => a.order - b.order);
  return { session, prompts: selected.map(({ text, timestamp }) => ({ text, timestamp })), skipped };
}
