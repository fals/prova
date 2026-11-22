package calc;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;

import org.junit.jupiter.api.Test;

class CalcTest {
    @Test
    void adds() {
        assertEquals(5, Calc.add(2, 3));
    }

    @Test
    void divides() {
        assertEquals(4, Calc.div(8, 2));
    }

    @Test
    void flakyClock() {
        assertNotEquals(0, (System.nanoTime() / 1000) % 3, "clock tick landed on a bad boundary");
    }

    @Test
    void slowIntegration() throws InterruptedException {
        Thread.sleep(1500);
    }

    @Test
    void hangsWhenAsked() throws InterruptedException {
        if (System.getenv("HANG") != null) {
            Thread.sleep(Long.MAX_VALUE);
        }
    }
}
