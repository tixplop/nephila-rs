package com.acme.core;

import java.util.List;

public class Externe {
    public static class Interne {
        public void ping() {
        }
    }

    private OrderRepository repo;

    public void viaLambda(List<Order> orders) {
        orders.forEach(o -> repo.save(o));
    }

    public int pick(int k) {
        return switch (k) {
            case 1 -> 10;
            default -> 0;
        };
    }

    public String text() {
        return """
            bloc de texte
            """;
    }
}
