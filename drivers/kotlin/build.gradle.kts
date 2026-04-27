plugins {
    kotlin("jvm") version "1.9.24"
    `maven-publish`
}

group = "dev.reddb"
version = "0.1.0"

kotlin {
    jvmToolchain(17)
}

repositories {
    mavenCentral()
}

val ktorVersion = "2.3.12"
val coroutinesVersion = "1.8.1"

dependencies {
    // Coroutines — the driver is suspend-fun-first.
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:$coroutinesVersion")

    // ktor-network for the redwire transport (raw TCP + TLS).
    api("io.ktor:ktor-network:$ktorVersion")
    api("io.ktor:ktor-network-tls:$ktorVersion")

    // ktor-client for HTTP transport.
    api("io.ktor:ktor-client-core:$ktorVersion")
    api("io.ktor:ktor-client-cio:$ktorVersion")

    // Jackson for JSON encode/decode of handshake + RPC payloads.
    api("com.fasterxml.jackson.module:jackson-module-kotlin:2.17.2")

    // zstd-jni — lazily loaded; tests run without it on the classpath.
    implementation("com.github.luben:zstd-jni:1.5.6-6")

    testImplementation(platform("org.junit:junit-bom:5.10.2"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testImplementation("org.junit.jupiter:junit-jupiter-params")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:$coroutinesVersion")
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("passed", "skipped", "failed")
        showStandardStreams = false
    }
    // SmokeTest is gated on RED_SMOKE=1.
    environment("RED_SMOKE", System.getenv("RED_SMOKE") ?: "0")
}

publishing {
    publications {
        create<MavenPublication>("mavenKotlin") {
            artifactId = "reddb-kotlin"
            from(components["java"])
        }
    }
}
