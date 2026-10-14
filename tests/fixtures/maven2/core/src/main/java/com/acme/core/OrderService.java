package com.acme.core;

import com.acme.domain.Order;

public class OrderService {
    private OrderRepository repo;

    public void place() {
        repo.save(new Order());
    }
}
