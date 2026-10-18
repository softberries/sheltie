package com.acme.shop.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class PricingTest {
    @Test
    fun grossAddsTax() {
        assertEquals(Money(1200), gross(listOf(Money(400), Money(600))))
    }
}
