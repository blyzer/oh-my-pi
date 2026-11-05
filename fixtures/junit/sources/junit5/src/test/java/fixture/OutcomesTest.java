package fixture;

import org.junit.jupiter.api.Assertions;
import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.Test;

class OutcomesTest {
    @Test void passes() { Assertions.assertEquals(2, 1 + 1); }
    @Test void fails() { Assertions.assertEquals(3, 1 + 1, "intentional failure"); }
    @Test @Disabled("intentional skip") void skipped() {}
    @Test void errors() { throw new IllegalStateException("intentional error"); }
}
