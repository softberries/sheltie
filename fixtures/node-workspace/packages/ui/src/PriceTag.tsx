import { formatPrice, type Money } from "@shop/utils";

export interface PriceTagProps {
  price: Money;
  strikethrough?: boolean;
}

export function PriceTag({ price, strikethrough = false }: PriceTagProps) {
  const text = formatPrice(price.cents, price.currency);
  return <span className={strikethrough ? "price price--old" : "price"}>{text}</span>;
}
