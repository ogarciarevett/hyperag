const MONTHS = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
/** A benchmark date as ISO UTC: LoCoMo "1:56 pm on 8 May, 2023" or LongMemEval "2023/05/30 (Tue) 23:40". */
export function isoDate(s?: string): string | undefined {
  const lo = s?.match(/(\d{1,2}):(\d{2})\s*(am|pm) on (\d{1,2}) ([a-z]+),? (\d{4})/i);
  if (lo) {
    const month = MONTHS.indexOf(lo[5]!.toLowerCase());
    const hour = (Number(lo[1]) % 12) + (lo[3]!.toLowerCase() === "pm" ? 12 : 0);
    if (month >= 0) return new Date(Date.UTC(Number(lo[6]), month, Number(lo[4]), hour, Number(lo[2]))).toISOString();
  }
  const lme = s?.match(/(\d{4})\/(\d{2})\/(\d{2})\s*\(\w+\)\s*(\d{2}):(\d{2})/);
  if (lme) return new Date(Date.UTC(Number(lme[1]), Number(lme[2]) - 1, Number(lme[3]), Number(lme[4]), Number(lme[5]))).toISOString();
  return undefined;
}
