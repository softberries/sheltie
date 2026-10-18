package com.acme.shop.api;

import com.acme.shop.core.Money;
import com.acme.shop.core.Pricing;
import java.util.ArrayList;
import java.util.List;

/** Places orders and prices them. */
public class OrderService {
    private final List<List<Money>> orders = new ArrayList<>();

    /** Places an order and returns its number. */
    public int place(List<Money> lines) {
        orders.add(List.copyOf(lines));
        return orders.size();
    }

    /** The gross total of an order. */
    public Money total(int order) {
        return Pricing.gross(orders.get(order - 1), Pricing.STANDARD_RATE);
    }
}
