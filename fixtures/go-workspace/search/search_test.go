package search

import (
	"os"
	"strings"
	"testing"
)

func TestFind(t *testing.T) {
	data, err := os.ReadFile("testdata/products.txt")
	if err != nil {
		t.Fatal(err)
	}
	var products []Product
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		sku, name, _ := strings.Cut(line, ",")
		products = append(products, Product{SKU: sku, Name: name})
	}
	if got := NewIndex(products).Find("TEA"); len(got) != 2 {
		t.Fatalf("Find(TEA) = %v, want 2 products", got)
	}
}
