import { OrdersClient } from "@shop/api-client";
import { PriceTag } from "@shop/ui";
import { cartTotal } from "./cart";

const client = new OrdersClient("/api");

export async function OrderSummary({ id }: { id: string }) {
  const order = await client.get(id);
  return <PriceTag price={cartTotal([order.total])} />;
}
