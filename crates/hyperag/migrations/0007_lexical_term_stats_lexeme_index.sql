-- Document frequency lookups read a few query terms across every readable scope. The primary key is
-- scope-first, so each term costs one scattered probe per scope (thousands of probes and tens of
-- thousands of pages per search). Lexeme-first and covering, the same lookup becomes one contiguous
-- index-only range per term. Additive and idempotent; no data changes and no table rewrite.
CREATE INDEX IF NOT EXISTS lexical_term_stats_lexeme
  ON lexical_term_stats(lexeme, scope) INCLUDE (df, valid_from, valid_to);
