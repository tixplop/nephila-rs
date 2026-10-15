package com.acme.core;

public enum Kinds {
    A,
    B
}

@interface Marker {
}

sealed interface Shape permits Square, Round {
}

final class Square implements Shape {
}

final class Round implements Shape {
}

sealed class Vehicle permits Car {
}

final class Car extends Vehicle {
}
