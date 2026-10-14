package com.acme.api;

import com.acme.core.OrderService;

public class ApiController {
    private OrderService service;

    public void create() {
        service.place();
    }
}
