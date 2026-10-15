package com.acme.core;

import com.acme.core.Order;

public class OrderService {
    private OrderRepository repo;

    public void place() {
        repo.save(new Order(1));
    }
}
