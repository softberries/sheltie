// Command shop prints a sample invoice and search result.
package main

import (
	"fmt"

	"example.com/shop/cart"
	"example.com/shop/invoice"
	"example.com/shop/pricing"
	"example.com/shop/search"
)

func main() {
	c := cart.New()
	c.Add(pricing.Line{SKU: "tea-1", Quantity: 2, Unit: 450})
	fmt.Println("cart", c.ID, "total", c.Total())

	inv := invoice.Invoice{Number: "2024-001", Lines: []pricing.Line{{SKU: "tea-1", Quantity: 2, Unit: 450}}}
	fmt.Print(invoice.Render(inv))

	ix := search.NewIndex([]search.Product{{SKU: "tea-1", Name: "Green tea"}})
	fmt.Println(ix.Find("tea"))
}
