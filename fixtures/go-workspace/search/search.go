// Package search finds products by name.
package search

import (
	"sort"
	"strings"

	"example.com/shop/internal/util"
)

// Product is a searchable product.
type Product struct {
	SKU  string
	Name string
}

// Index is an in-memory product index.
type Index struct {
	products []Product
}

// NewIndex indexes the products.
func NewIndex(products []Product) *Index {
	return &Index{products: products}
}

// Find returns the products whose name contains the query, sorted by SKU.
func (ix *Index) Find(query string) []Product {
	q := util.Fold(query)
	var out []Product
	for _, p := range ix.products {
		if strings.Contains(util.Fold(p.Name), q) {
			out = append(out, p)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].SKU < out[j].SKU })
	return out
}
