import { createHash } from "node:crypto";
export type Source = "claude" | "codex";
export const hash = (s: string) => createHash("sha256").update(s).digest("hex");

// Conservative local redaction intentionally removes some non-secret opaque identifiers too.
export function redact(text: string) {
  return text
    .replace(/-----BEGIN [\w ]*PRIVATE KEY-----[\s\S]*?-----END [\w ]*PRIVATE KEY-----/g, "[REDACTED_PRIVATE_KEY]")
    .replace(/^.*(?:\b(?:argv|args|arguments|process[_ ]?args|command[_ ]?line|environment[_ ]?dump)\s*[:=]|^\s*(?:export\s+)?[A-Z_][A-Z0-9_]*=).*$/gm, "[REDACTED_PROCESS_OR_ENV]")
    .replace(/^\s*(?:[$>]\s*)?(?:\/[\w./-]+\/)?(?:env|printenv|ps|curl|ssh|docker|node|bun|python3?|codex|claude|hyperag|engram)\s+.*$/gm, "[REDACTED_PROCESS_OR_ENV]")
    .replace(/\b(Bearer|Basic)\s+[\w.+/=:-]+/gi, "$1 [REDACTED]")
    .replace(/((?:[\w-]*(?:token|secret|password|passwd|api[_-]?key|access[_-]?key|credential)[\w-]*)["']?\s*(?:[:=]|\bis\b)\s*)(?:"[^"\n]*"|'[^'\n]*'|[^\s,;]+)/gi, "$1[REDACTED]")
    .replace(/(https?:\/\/)[^\s/@]+:[^\s/@]+@/gi, "$1[REDACTED]@")
    .replace(/--(?:token|password|secret|api-key|key)\s+(?:"[^"\n]*"|'[^'\n]*'|[^\s]+)/gi, "[REDACTED_CREDENTIAL_ARG]")
    .replace(/\b(?:AKIA|ASIA)[A-Z0-9]{16}\b|\beyJ[\w-]+\.[\w-]+\.[\w-]+\b/g, "[REDACTED_TOKEN]")
    .replace(/\b(?:sk-|ghp_|github_pat_|xox[baprs]-)[\w-]{10,}/g, "[REDACTED_TOKEN]")
    .replace(/\+\d[\d ().-]{7,}\d|(?:\(\d{3}\)\s*|\b\d{3}[ .-])\d{3}[ .-]\d{4}\b/g, "[REDACTED_PHONE]")
    .replace(/\b[A-Za-z0-9_+/=-]{32,}\b/g, value => /[a-zA-Z]/.test(value) && /\d/.test(value) ? "[REDACTED_OPAQUE]" : value);
}

type Prompt = { text: string; timestamp: string | null };
export function parseTranscript(source: Source, text: string) {
  const session = { id: null as string | null, cwd: null as string | null, startedAt: null as string | null, endedAt: null as string | null, lastEventAt: null as string | null, model: null as string | null };
  const prompts: Prompt[] = [], fallback: Prompt[] = [], skipped: Record<string, number> = {};
  const skip = (why: string) => { skipped[why] = (skipped[why] ?? 0) + 1; };
  const string = (v: unknown) => typeof v === "string" ? v : null;
  const stamp = (v: unknown) => typeof v === "string" && Number.isFinite(Date.parse(v)) ? v : null;
  const add = (target: Prompt[], raw: unknown, timestamp: unknown) => {
    const content = typeof raw === "string" ? raw : Array.isArray(raw) ? raw.filter(b => b && ["text", "input_text"].includes(b.type) && typeof b.text === "string").map(b => b.text).join("\n") : "";
    if (!content.trim()) { skip("nonText"); return; }
    if (content.length > 16_000) { skip("largePrompt"); return; }
    if (/^\s*(?:# AGENTS\.md instructions|<environment_context>|<permissions instructions>|<local-command|<system-reminder>)/.test(content)) { skip("generatedContext"); return; }
    target.push({ text: redact(content), timestamp: stamp(timestamp) });
  };
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    if (line.length > 256_000) { skip("largeRecord"); continue; }
    let r: any;
    try { r = JSON.parse(line); if (!r || typeof r !== "object") throw new Error(); } catch { skip("invalidJson"); continue; }
    const p = r.payload ?? {}, timestamp = stamp(r.timestamp);
    if (timestamp) {
      if (!session.startedAt || Date.parse(timestamp) < Date.parse(session.startedAt)) session.startedAt = timestamp;
      if (!session.lastEventAt || Date.parse(timestamp) > Date.parse(session.lastEventAt)) session.lastEventAt = timestamp;
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
  // Codex mirrors user messages into event_msg: use that stream only for old files without response items.
  const selected = prompts.length ? prompts : source === "codex" ? fallback : prompts;
  const seen = new Set<string>();
  const unique = selected.filter(p => { const key = hash(JSON.stringify(p)); if (seen.has(key)) { skip("duplicatePrompt"); return false; } seen.add(key); return true; });
  if (source === "codex" && prompts.length && fallback.length) skipped.mirroredUserEvent = fallback.length;
  return { session, prompts: unique, skipped };
}
