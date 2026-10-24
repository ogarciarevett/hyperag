# LLM-scored memory benchmarks: item counts and call estimates (2026-10-23)

Same open model as generator and judge for every system. Counts are from the pinned public files.

| Benchmark | Source (pinned) | Questions | Memory to ingest | Calls per system (answer + judge) |
| --- | --- | --- | --- | --- |
| LoCoMo | snap-research/locomo `data/locomo10.json` | 1,986 (cat 1: 282, 2: 321, 3: 96, 4: 841, 5 adversarial: 446); 1,540 without cat 5 | 10 conversations, 272 sessions, 5,882 turns, 0.73 M chars | 3,972 (3,080 without cat 5) |
| LongMemEval S (cleaned) | xiaowu0162/longmemeval-cleaned @98d7416 `longmemeval_s_cleaned.json` | 500 (30 abstention) | per question its own haystack: 47.7 sessions, 493.5 turns; 246,750 turns, 245 M chars in total | 1,000 |
| LongMemEval oracle | same, `longmemeval_oracle.json` | 500 | 1.9 sessions per question | 1,000 |
| MSC (DMR, as in the MemGPT paper) | MemGPT/MSC-Self-Instruct @5138f41 | 500 | 4 prior sessions each, 25,459 turns | 1,000 |

Token estimate for a retrieval system (top 10 memories, about 2,000 input and 60 output tokens per answer, about 400 input and 20 output per judge call): LoCoMo about 4.8 M input, LongMemEval S 1.2 M, oracle 1.2 M, DMR 1.2 M; about 8.4 M input and 0.3 M output tokens per system for all four.

Not included above, and much larger:
- Systems that call the LLM at ingestion (Mem0, Zep/Graphiti, Letta, Cognee, LangMem with extraction ON): roughly one call per turn or per session. LoCoMo 5,882 turns; DMR 25,459 turns; LongMemEval S 246,750 turns (23,867 sessions), which is far above the others and should be run per session, or on a question subset agreed in advance.
- A full-context baseline on LongMemEval S (about 115 k tokens per question, 57 M input tokens): not planned.

Hyperag itself makes no LLM call at ingestion. Its only cost on the GPU is the shared answer and judge calls. Embedding LongMemEval S (245 M chars) on this Mac's CPU would take hours; it is the one set that may move to the rented GPU for embedding.
