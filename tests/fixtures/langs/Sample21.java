package com.acme.shop;

import java.util.List;
import static java.util.Objects.requireNonNull;

public sealed interface Sample21 permits Sample21.A, Sample21.B {
    record A(String name, int qty) implements Sample21 {
        A {
            requireNonNull(name);
        }
    }
    record B(List<String> items) implements Sample21 {}
    enum Kind { X, Y }
    @interface Marker { String value() default ""; }

    static String describe(Sample21 s) {
        return switch (s) {
            case A a when a.qty() > 3 -> "many " + a.name();
            case A a -> a.name();
            case B(var items) -> String.join(",", items);
        };
    }

    default Runnable task() {
        var text = """
            hello
            """;
        return () -> System.out.println(text);
    }
}
