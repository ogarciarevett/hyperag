/** Read the shipped eligibility SQL without depending on Rust's declaration layout. */
export function extractRankable(source: string): string {
  const match = source.match(/\bconst\s+RANKABLE\s*:\s*&\s*str\s*=\s*"([\s\S]*?)"\s*;/);
  if (!match) throw new Error("Rust RANKABLE string constant was not found");
  return match[1]!;
}
