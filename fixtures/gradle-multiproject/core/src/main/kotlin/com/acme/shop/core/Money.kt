package com.acme.shop.core

import kotlin.math.roundToLong

/** An amount of money in cents. */
data class Money(val cents: Long) : Comparable<Money> {
    /** The sum of two amounts. */
    operator fun plus(other: Money): Money {
        return Money(cents + other.cents)
    }

    /** A percentage of this amount, rounded half up. */
    fun percent(pct: Int): Money {
        return Money((cents * pct / 100.0).roundToLong())
    }

    override fun compareTo(other: Money): Int = cents.compareTo(other.cents)

    companion object {
        /** No money. */
        @JvmField
        val ZERO = Money(0)
    }
}
