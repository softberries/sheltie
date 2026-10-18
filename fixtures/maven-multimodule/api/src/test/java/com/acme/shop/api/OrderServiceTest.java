package com.acme.shop.api;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.acme.shop.core.Money;
import java.util.List;
import org.junit.jupiter.api.Test;

class OrderServiceTest {
    @Test
    void totalsAnOrder() {
        var service = new OrderService();
        int order = service.place(List.of(new Money(500)));
        assertEquals(new Money(600), service.total(order));
    }
}
