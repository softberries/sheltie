// Package pricing turns order lines into totals.
package pricing

// Rate is the standard tax rate, in percent.
const Rate = 20

// Money is an amount in cents.
type Money int64

// Line is one order line.
type Line struct {
	SKU      string
	Quantity int
	Unit     Money
}

// Net is the line's price before tax.
func (l Line) Net() Money {
	return l.Unit * Money(l.Quantity)
}

// Total is the gross total of the lines, tax included.
func Total(lines []Line) Money {
	var net Money
	for _, l := range lines {
		net += l.Net()
	}
	return net + percent(net, Rate)
}

// percent applies a percentage, rounding half away from zero.
func percent(m Money, pct int64) Money {
	v := int64(m) * pct
	if v >= 0 {
		return Money((v + 50) / 100)
	}
	return Money((v - 50) / 100)
}
