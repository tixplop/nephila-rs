package com.alpha;

public class Cart {
    private Order order;
    private Line line;

    public void run() {
        order.run();
        line.run();
    }
}
