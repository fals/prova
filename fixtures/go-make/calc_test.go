package calc

import (
	"os"
	"testing"
	"time"
)

func TestAdd(t *testing.T) {
	if got := Add(2, 3); got != 5 {
		t.Fatalf("Add(2, 3) = %d, want 5", got)
	}
}

func TestDiv(t *testing.T) {
	if got := Div(8, 2); got != 4 {
		t.Fatalf("Div(8, 2) = %d, want 4", got)
	}
}

func TestFlakyClock(t *testing.T) {
	if time.Now().UnixNano()/1000%3 == 0 {
		t.Fatal("clock tick landed on a bad boundary")
	}
}

func TestSlowIntegration(t *testing.T) {
	time.Sleep(1500 * time.Millisecond)
}

func TestHang(t *testing.T) {
	if os.Getenv("HANG") == "" {
		t.Skip("set HANG=1 to hang")
	}
	select {}
}
