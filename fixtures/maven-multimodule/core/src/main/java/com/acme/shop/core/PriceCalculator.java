package com.acme.shop.core;

import java.util.List;

/** Totals and tax for a list of line amounts. */
public final class PriceCalculator {
    /** Standard VAT rate, in percent. */
    public static final int STANDARD_RATE = 20;

    private final int rate;

    public PriceCalculator() {
        this(STANDARD_RATE);
    }

    public PriceCalculator(int rate) {
        this.rate = rate;
    }

    /** The net total of the lines. */
    public Money net(List<Money> lines) {
        Money total = Money.ZERO;
        for (Money line : lines) {
            total = total.plus(line);
        }
        return total;
    }

    /** The gross total: net plus tax. */
    public Money gross(List<Money> lines) {
        Money net = net(lines);
        return net.plus(tax(net));
    }

    private Money tax(Money net) {
        return net.percent(rate);
    }
}
