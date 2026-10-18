package com.acme.shop.core;

/** An amount of money in cents. */
public record Money(long cents) implements Comparable<Money> {
    /** No money. */
    public static final Money ZERO = new Money(0);

    /** The sum of two amounts. */
    public Money plus(Money other) {
        return new Money(cents + other.cents);
    }

    /** A percentage of this amount, rounded half up. */
    public Money percent(int pct) {
        return new Money(Math.round(cents * pct / 100.0));
    }

    @Override
    public int compareTo(Money other) {
        return Long.compare(cents, other.cents);
    }
}
