package cart_test

import (
	"testing"

	"example.com/shop/cart"
	"example.com/shop/pricing"
)

func TestAddMergesLines(t *testing.T) {
	c := cart.New()
	c.Add(pricing.Line{SKU: "tea", Quantity: 1, Unit: 250})
	c.Add(pricing.Line{SKU: "tea", Quantity: 1, Unit: 250})
	if got := c.Total(); got != 600 {
		t.Fatalf("Total = %d, want 600", got)
	}
}
