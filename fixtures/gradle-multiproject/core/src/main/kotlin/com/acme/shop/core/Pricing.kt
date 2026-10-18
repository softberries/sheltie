@file:JvmName("Pricing")

package com.acme.shop.core

/** Standard VAT rate, in percent. */
const val STANDARD_RATE = 20

/** The net total of the lines. */
fun net(lines: List<Money>): Money {
    var total = Money.ZERO
    for (line in lines) {
        total += line
    }
    return total
}

/** The gross total: net plus tax. */
fun gross(lines: List<Money>, rate: Int = STANDARD_RATE): Money {
    val net = net(lines)
    return net + tax(net, rate)
}

/** The tax on an amount at the standard rate. */
fun standardTax(amount: Money) = tax(amount, STANDARD_RATE)

private fun tax(net: Money, rate: Int): Money = net.percent(rate)
