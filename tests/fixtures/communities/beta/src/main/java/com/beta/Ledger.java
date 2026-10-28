package com.beta;

public class Ledger {
    private Invoice invoice;
    private Entry entry;

    public void run() {
        invoice.run();
        entry.run();
    }
}
