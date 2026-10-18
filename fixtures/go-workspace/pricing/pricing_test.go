package pricing

import "testing"

func TestTotal(t *testing.T) {
	lines := []Line{{SKU: "tea", Quantity: 2, Unit: 250}}
	if got := Total(lines); got != 600 {
		t.Fatalf("Total = %d, want 600", got)
	}
}

func TestPercentRounds(t *testing.T) {
	if got := percent(5, 50); got != 3 {
		t.Fatalf("percent = %d, want 3", got)
	}
}
