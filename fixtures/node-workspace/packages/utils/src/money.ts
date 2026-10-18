/** Currencies the shop sells in. */
export type Currency = "EUR" | "USD" | "PLN";

/** An amount of money in minor units. */
export interface Money {
  cents: number;
  currency: Currency;
}

export const toMajor = (cents: number): number => {
  return cents / 100;
};
