// Package cart holds a customer's lines before checkout.
package cart

import (
	"github.com/google/uuid"

	"example.com/shop/pricing"
)

// Cart is a customer's open order.
type Cart struct {
	ID    string
	lines []pricing.Line
}

// New returns an empty cart with a fresh id.
func New() *Cart {
	return &Cart{ID: uuid.NewString()}
}

// Add puts a line in the cart, merging it with a line for the same SKU.
func (c *Cart) Add(line pricing.Line) {
	for i := range c.lines {
		if c.lines[i].SKU == line.SKU {
			c.lines[i].Quantity += line.Quantity
			return
		}
	}
	c.lines = append(c.lines, line)
}

// Total is the cart's gross total.
func (c *Cart) Total() pricing.Money {
	return pricing.Total(c.lines)
}
