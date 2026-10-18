// Package invoice renders invoices.
package invoice

import (
	"fmt"
	"strings"

	"example.com/shop/internal/util"
	"example.com/shop/pricing"
)

// Invoice is a rendered invoice.
type Invoice struct {
	Number string
	Lines  []pricing.Line
}

// Render prints the invoice as plain text.
func Render(inv Invoice) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Invoice %s\n", inv.Number)
	for _, l := range inv.Lines {
		fmt.Fprintf(&b, "%-12s %3d x %s\n", l.SKU, l.Quantity, util.Cents(int64(l.Unit)))
	}
	fmt.Fprintf(&b, "Total (incl. %d%% tax): %s\n", pricing.Rate, util.Cents(int64(pricing.Total(inv.Lines))))
	return b.String()
}
