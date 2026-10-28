package com.alpha;

public class Order {
    private Line line;
    private Price price;

    public void run() {
        line.run();
        price.run();
    }
}
