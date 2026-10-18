package com.acme.shop.core;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.List;
import org.junit.jupiter.api.Test;

class PriceCalculatorTest {
    @Test
    void grossAddsTax() {
        var calc = new PriceCalculator();
        assertEquals(new Money(1200), calc.gross(List.of(new Money(400), new Money(600))));
    }
}
