import { describe, expect, it } from "vitest";
import orders from "./fixtures/orders.json";
import { OrdersClient } from "./client";

describe("OrdersClient", () => {
  it("has fixtures", () => {
    expect(orders).toHaveLength(2);
    expect(new OrdersClient("http://localhost")).toBeTruthy();
  });
});
