import type { Money } from "@shop/utils";

export interface Order {
  id: string;
  total: Money;
}

export class OrdersClient {
  constructor(private readonly baseUrl: string) {}

  async get(id: string): Promise<Order> {
    const res = await fetch(`${this.baseUrl}/orders/${encodeURIComponent(id)}`);
    if (!res.ok) {
      throw new Error(`order ${id}: ${res.status}`);
    }
    return (await res.json()) as Order;
  }
}
