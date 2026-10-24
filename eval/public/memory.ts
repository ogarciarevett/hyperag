// Agent-memory benchmarks through any system adapter: each conversation is a namespace, ingested one
// session per ingest() call (so extraction systems extract per session), then every question
// retrieves top k memories, the shared model answers from them, and (LongMemEval, DMR) the same
// model judges. Prompts are the benchmarks' own:
//   LoCoMo       snap-research/locomo@3eb6f2c task_eval/gpt_utils.py (RAG path: context + QA_PROMPT, cat 2 and
//                5 rules; retrieval with the question only); the cat 5 option order uses a seeded coin.
//                scored offline by score_locomo.py (the official F1 / adversarial rule).
//   LongMemEval  xiaowu0162/LongMemEval@9e0b455 run_generation.py (non-CoT template) and
//                evaluate_qa.py (type-specific yes/no judge, temperature 0, max_tokens 10).
//                Retrieved memories are joined in rank order, not wrapped per session by date as
//                run_generation.py does for session chunks (systems return memories, not sessions).
//   DMR          MSC-Self-Instruct as in the MemGPT paper; LongMemEval's answer template and its
//                default judge template (no official judge prompt was published).
//
//   bun eval/public/memory.ts --system mem0 --bench locomo|lme-s|lme-s100|lme-oracle|dmr [--workers 4] [--limit 2]
// lme-s is the full 500; lme-s100 the seeded subset for systems that call the LLM at ingestion
// ("LME-S subset n=100", never mixed with lme-s). --ingest-only fills a resumable store ahead of time.
// Needs EVAL_LLM_BASE_URL, EVAL_LLM_MODEL, EVAL_LLM_API_KEY (the one shared endpoint) unless --ingest-only.
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";

import type { Doc, MemorySystem, SystemFactory } from "./system";
import { isoDate } from "./dates";
import { childrenRssBytes } from "./rss";
import { pyAdapter } from "./systems/py";

const { values: a } = parseArgs({
  options: {
    system: { type: "string" },
    profile: { type: "string", default: "default" },
    bench: { type: "string" },
    data: { type: "string", default: "data/memory" },
    out: { type: "string", default: "eval/public/results/memory" },
    k: { type: "string", default: "10" },
    workers: { type: "string", default: "4" },
    concurrency: { type: "string", default: "16" },
    limit: { type: "string" },
    "ingest-only": { type: "boolean", default: false },
  },
});
if (!a.system || !a.bench) throw new Error("--system and --bench are required");
const k = Number(a.k);

type Question = { id: string; question: string; prompt: string; gold: string; type: string; asOf?: string; extra?: Record<string, unknown> };
type Unit = { ns: string; sessions: Doc[][]; questions: Question[] };
/** Retrieved memories in rank order up to 90,000 characters (about 22,500 tokens), so every prompt
 * fits the endpoint's 32,768-token context with room for the answer; the same cap for every system. */
const fit = (memories: string[]) => {
  let total = 0;
  return memories.filter((m) => (total += m.length + 2) <= 90_000);
};
/** Every system gets turns cut at 15,000 characters, as beir.ts cuts documents (20 of 246,750 LME-S turns are longer). */
const cut = (text: string) => text.slice(0, 15_000);

// ---- benchmark loading ----------------------------------------------------------------------
const LOCOMO_QA = (q: string) => `\nBased on the above context, write an answer in the form of a short phrase for the following question. Answer with exact words from the context whenever possible.\n\nQuestion: ${q} Short answer:\n`;
const LOCOMO_QA_5 = (q: string) => `\nBased on the above context, answer the following question.\n\nQuestion: ${q} Short answer:\n`;
const LME_QA = (history: string, date: string, q: string) =>
  `I will give you several history chats between you and a user. Please answer the question based on the relevant chat history.\n\n\nHistory Chats:\n\n${history}\n\nCurrent Date: ${date}\nQuestion: ${q}\nAnswer:`;
/** Seeded order (sha256 of seed and id), so a subset is the same for every system and every run. */
const seeded = <T>(xs: T[], id: (x: T) => string, seed = "42") =>
  [...xs].sort((p, q) => (createHash("sha256").update(seed + id(p)).digest("hex") < createHash("sha256").update(seed + id(q)).digest("hex") ? -1 : 1));

function locomo(): Unit[] {
  const data = JSON.parse(readFileSync(`${a.data}/locomo10.json`, "utf8")) as Array<Record<string, any>>;
  return data.map((s) => {
    const c = s.conversation;
    const sessions: Doc[][] = [];
    for (let i = 1; c[`session_${i}`]; i++) {
      const date = c[`session_${i}_date_time`] as string;
      // the official RAG context line: "<date>: <speaker> said, "<text>"" (and shared <caption>)
      sessions.push(c[`session_${i}`].map((t: any) => ({ id: t.dia_id, speaker: t.speaker, timestamp: date,
        text: cut(`${date}: ${t.speaker} said, "${t.text}"${t.blip_caption ? ` and shared ${t.blip_caption}` : ""}`) })));
    }
    // the questions come after the last session, so its date anchors relative dates
    const asOf = isoDate(sessions.at(-1)?.[0]?.timestamp);
    const questions = (s.qa as any[]).map((qa, i): Question => {
      const id = `${s.sample_id}:${i}`;
      if (qa.category === 5) {
        // the official script flips a coin per question; a seeded coin keeps it identical across systems
        const notFirst = createHash("sha256").update(`42${id}`).digest()[0]! < 128;
        const opts = notFirst ? ["Not mentioned in the conversation", qa.adversarial_answer] : [qa.adversarial_answer, "Not mentioned in the conversation"];
        return { id, question: qa.question, prompt: `${qa.question} Select the correct answer: (a) ${opts[0]} (b) ${opts[1]}. `, gold: String(qa.adversarial_answer ?? ""), type: "5", asOf, extra: { options: { a: opts[0], b: opts[1] }, evidence: qa.evidence } };
      }
      const q = qa.category === 2 ? `${qa.question} Use DATE of CONVERSATION to answer with an approximate date.` : qa.question;
      return { id, question: qa.question, prompt: q, gold: String(qa.answer), type: String(qa.category), asOf, extra: { evidence: qa.evidence } };
    });
    return { ns: `locomo/${s.sample_id}`, sessions, questions };
  });
}

function longmemeval(set: string, file: string, subset?: number): Unit[] {
  let data = JSON.parse(readFileSync(`${a.data}/${file}`, "utf8")) as Array<Record<string, any>>;
  if (subset) data = seeded(data, (x) => x.question_id).slice(0, subset);
  return data.map((e) => ({
    ns: `${set}/${e.question_id}`,  // the subset reuses the full set's namespaces
    sessions: (e.haystack_sessions as any[][]).map((turns, s) =>
      turns.map((t, j) => ({ id: `${e.haystack_session_ids[s]}:${j}`, speaker: t.role, timestamp: e.haystack_dates[s], text: cut(`[${e.haystack_dates[s]}] ${t.role}: ${t.content}`) }))),
    questions: [{ id: e.question_id, question: e.question, prompt: e.question, gold: String(e.answer), type: e.question_id.endsWith("_abs") ? "abstention" : e.question_type, asOf: isoDate(e.question_date),
      extra: { question_date: e.question_date, task: e.question_type, answer_sessions: e.answer_session_ids } }],
  }));
}

function dmr(): Unit[] {
  const rows = readFileSync(`${a.data}/msc_self_instruct.jsonl`, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  return rows.map((r, i) => ({
    ns: `dmr/${i}`,
    sessions: (r.previous_dialogs as any[]).map((d, s) =>
      (d.dialog as any[]).map((t, j) => {
        const speaker = j % 2 === 0 ? "Speaker 1" : "Speaker 2";
        return { id: `${i}:${s}:${j}`, speaker, timestamp: d.time_back, text: cut(`[${d.time_back}] ${speaker}: ${t.text}`) };
      })),
    questions: [{ id: `dmr-${i}`, question: r.self_instruct.B, prompt: r.self_instruct.B, gold: r.self_instruct.A, type: "dmr" }],
  }));
}

const benches: Record<string, () => Unit[]> = {
  locomo,
  "lme-s": () => longmemeval("lme-s", "longmemeval_s_cleaned.json"),
  "lme-s100": () => longmemeval("lme-s", "longmemeval_s_cleaned.json", 100),
  "lme-oracle": () => longmemeval("lme-oracle", "longmemeval_oracle.json"),
  dmr,
};
const units = benches[a.bench]?.() ?? [];
if (!units.length) throw new Error("--bench is locomo, lme-s, lme-s100, lme-oracle or dmr");
const asked = a.limit ? units.slice(0, Number(a.limit)) : units;
// lme-oracle retrieves top k over the oracle haystack; it is not LongMemEval's oracle-session setting
const benchLabel = { "lme-s": "LME-S (500)", "lme-s100": "LME-S subset n=100", "lme-oracle": "LME oracle haystack, top-10 retrieval" }[a.bench] ?? a.bench;

// ---- the shared model ------------------------------------------------------------------------
const LLM = { url: process.env.EVAL_LLM_BASE_URL, model: process.env.EVAL_LLM_MODEL, key: process.env.EVAL_LLM_API_KEY ?? "none" };
if (!a["ingest-only"] && (!LLM.url || !LLM.model)) throw new Error("EVAL_LLM_BASE_URL and EVAL_LLM_MODEL are required");
const usage = { calls: 0, prompt: 0, completion: 0 };
let inflight = 0;
const waiters: Array<() => void> = [];
async function chat(prompt: string, maxTokens: number): Promise<string> {
  while (inflight >= Number(a.concurrency)) await new Promise<void>((r) => waiters.push(r));
  inflight++;
  try {
    for (let attempt = 0; ; attempt++) {
      const r = await fetch(`${LLM.url}/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${LLM.key}` },
        body: JSON.stringify({ model: LLM.model, messages: [{ role: "user", content: prompt }], temperature: 0, max_tokens: maxTokens }),
      }).catch((e: Error) => e);
      if (r instanceof Response && r.ok) {
        const body = (await r.json()) as { choices: Array<{ message: { content: string } }>; usage?: { prompt_tokens: number; completion_tokens: number } };
        usage.calls++;
        usage.prompt += body.usage?.prompt_tokens ?? 0;
        usage.completion += body.usage?.completion_tokens ?? 0;
        return body.choices[0]!.message.content ?? "";
      }
      // about 10 minutes of retries, so a planned restart of the endpoint does not end a run
      if (attempt === 12) throw new Error(`LLM call failed: ${r instanceof Response ? `${r.status} ${await r.text()}` : r.message}`);
      await Bun.sleep(Math.min(60_000, 2000 * 2 ** attempt));
    }
  } finally {
    inflight--;
    waiters.shift()?.();
  }
}

function judgePrompt(q: Question, response: string): string {
  // LongMemEval evaluate_qa.py get_anscheck_prompt, verbatim
  const task = (q.extra?.task as string | undefined) ?? "multi-session";
  const tail = `\n\nQuestion: ${q.question}\n\nCorrect Answer: ${q.gold}\n\nModel Response: ${response}\n\nIs the model response correct? Answer yes or no only.`;
  const base = "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no.";
  const full = `${base} If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no.`;
  if (q.type === "abstention")
    return `I will give you an unanswerable question, an explanation, and a response from a model. Please answer yes if the model correctly identifies the question as unanswerable. The model could say that the information is incomplete, or some other information is given but the asked information is not.\n\nQuestion: ${q.question}\n\nExplanation: ${q.gold}\n\nModel Response: ${response}\n\nDoes the model correctly identify the question as unanswerable? Answer yes or no only.`;
  if (task === "temporal-reasoning")
    return `${full} In addition, do not penalize off-by-one errors for the number of days. If the question asks for the number of days/weeks/months, etc., and the model makes off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response is still correct. ${tail}`;
  if (task === "knowledge-update")
    return `${base} If the response contains some previous information along with an updated answer, the response should be considered as correct as long as the updated answer is the required answer.${tail}`;
  if (task === "single-session-preference")
    return `I will give you a question, a rubric for desired personalized response, and a response from a model. Please answer yes if the response satisfies the desired response. Otherwise, answer no. The model does not need to reflect all the points in the rubric. The response is correct as long as it recalls and utilizes the user's personal information correctly.\n\nQuestion: ${q.question}\n\nRubric: ${q.gold}\n\nModel Response: ${response}\n\nIs the model response correct? Answer yes or no only.`;
  return `${full} ${tail}`;
}

// ---- run ---------------------------------------------------------------------------------------
const factory = existsSync(`${import.meta.dir}/systems/${a.system}.ts`)
  ? ((await import(`./systems/${a.system}.ts`)).default as SystemFactory)
  : pyAdapter(a.system);
const workers: MemorySystem[] = await Promise.all(Array.from({ length: Number(a.workers) }, () => factory(a.profile!)));
const meta = await workers[0]!.describe();
const tag = `${a.bench}.${meta.system}.${meta.profile.replaceAll("+", "_")}${a.limit ? `.limit${a.limit}` : ""}${a["ingest-only"] ? ".ingest" : ""}`;
mkdirSync(a.out!, { recursive: true });
const outFile = `${a.out}/${tag}.jsonl`;
writeFileSync(outFile, "");

const ingestMs: number[] = [];
const searchMs: number[] = [];
let systemRssPeak = 0;
// an adapter that does not report its RAM is measured as this process's children (its servers)
const sample = async (s: MemorySystem) => (systemRssPeak = Math.max(systemRssPeak, s.rssBytes ? await s.rssBytes() : childrenRssBytes()));
let next = 0;
const t0 = performance.now();
await Promise.all(
  workers.map(async (s) => {
    for (let u = asked[next++]; u; u = asked[next++]) {
      await s.open(u.ns);
      const ti = performance.now();
      for (const session of u.sessions) await s.ingest(u.ns, session);
      ingestMs.push(performance.now() - ti);
      await sample(s);
      if (a["ingest-only"]) {
        console.log(`${u.ns}: ingested (${next}/${asked.length})`);
        continue;
      }
      await Promise.all(
        u.questions.map(async (q) => {
          const ts = performance.now();
          let searchError: string | undefined;
          // a search the system itself fails on answers from no memories and is listed
          const hits = await s.search(u.ns, q.question, k, q.asOf ? { asOf: q.asOf } : undefined).catch((e: Error) => {
            if (/adapter (exited|protocol)/.test(e.message)) throw e;  // a broken bridge ends the run
            return (searchError = e.message.slice(0, 300)), [];
          });
          searchMs.push(performance.now() - ts);
          const memories = fit(hits.map((h) => h.text ?? "").filter(Boolean));
          let prompt: string;
          if (a.bench === "locomo") {
            // the RAG path: get_rag_context(...) + "\n\n" + QA_PROMPT, no conversation header
            prompt = memories.join("\n") + "\n\n" + (q.type === "5" ? LOCOMO_QA_5(q.prompt) : LOCOMO_QA(q.prompt));
          } else {
            prompt = LME_QA(memories.join("\n\n"), (q.extra?.question_date as string | undefined) ?? "", q.prompt);
          }
          const answer = await chat(prompt, a.bench === "locomo" ? 32 : 500);
          // a failed search scores 0 whatever the model answers (score_locomo.py does the same)
          const judged = a.bench === "locomo" ? undefined : searchError ? false : (await chat(judgePrompt(q, answer), 10)).toLowerCase().includes("yes");
          appendFileSync(outFile, `${JSON.stringify({ id: q.id, type: q.type, question: q.question, gold: q.gold, answer, correct: judged, retrieved: hits.map((h) => h.id), ...(searchError ? { searchError } : {}), extra: q.extra })}\n`);
        }),
      );
      console.log(`${u.ns}: ${u.questions.length} questions (${next}/${asked.length})`);
    }
    await sample(s);
    await s.close();
  }),
);
const pct = (xs: number[], p: number) => [...xs].sort((x, y) => x - y)[Math.min(xs.length - 1, Math.ceil(p * xs.length) - 1)];
const rows = readFileSync(outFile, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as { type: string; correct?: boolean; searchError?: string });
const byType: Record<string, { n: number; correct: number }> = {};
for (const r of rows) {
  const b = (byType[r.type] ??= { n: 0, correct: 0 });
  b.n++;
  b.correct += r.correct ? 1 : 0;
}
writeFileSync(
  `${a.out}/${tag}.meta.json`,
  JSON.stringify(
    { ...meta, transportRetry: process.env.EVAL_GPU_TRANSPORT_NOTE ?? "pre-common-proxy policy; see run notes", bench: benchLabel, units: asked.length, asOfUse: (meta.config as Record<string, unknown>).asOf ?? "none (no date parameter at search)", questions: rows.length, expectedQuestions: asked.reduce((n, u) => n + u.questions.length, 0), failedSearches: rows.filter((r) => r.searchError).length, k, workers: Number(a.workers), llm: { model: LLM.model, ...usage },
      accuracy: a.bench === "locomo" ? "score_locomo.py" : rows.filter((r) => r.correct).length / rows.length, byType: a.bench === "locomo" ? undefined : byType,
      wallSeconds: (performance.now() - t0) / 1000, ingestP50Ms: pct(ingestMs, 0.5), searchP50Ms: pct(searchMs, 0.5), searchP95Ms: pct(searchMs, 0.95),
      rssPeakBytes: process.memoryUsage().rss, systemRssPeakBytes: systemRssPeak, date: new Date().toISOString() },
    null,
    2,
  ),
);
console.log(`${tag}: ${rows.length} questions, ${usage.calls} LLM calls, ${usage.prompt} prompt + ${usage.completion} completion tokens`);
