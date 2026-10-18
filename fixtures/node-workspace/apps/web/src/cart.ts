import type { PriceTagProps } from "@shop/ui";

type Money = PriceTagProps["price"];

export function cartTotal(items: Money[]): Money {
  const cents = items.reduce((sum, m) => sum + m.cents, 0);
  return { cents, currency: items[0]?.currency ?? "EUR" };
}
