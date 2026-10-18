package com.acme.shop.app;

import com.acme.shop.api.OrderService;
import com.acme.shop.core.Money;
import java.util.List;

/** Command-line entry point. */
public final class App {
    private App() {}

    public static void main(String[] args) {
        var service = new OrderService();
        int order = service.place(List.of(new Money(1999), new Money(500)));
        System.out.println("Order " + order + ": " + service.total(order).cents() + " cents");
    }
}
