import { type Currency, toMajor } from "./money";

/** Formats an amount in minor units, e.g. `formatPrice(1999, "EUR")` is "19.99 EUR". */
export function formatPrice(cents: number, currency: Currency): string {
  const major = toMajor(cents);
  return `${major.toFixed(2)} ${currency}`;
}

// Rounds half away from zero, in minor units.
function roundHalfAway(value: number): number {
  return Math.sign(value) * Math.round(Math.abs(value));
}

/** Applies a percentage discount to an amount in minor units. */
export function applyDiscount(cents: number, percent: number): number {
  return roundHalfAway(cents - (cents * percent) / 100);
}
