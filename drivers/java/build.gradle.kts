plugins {
    `java-library`
    `maven-publish`
}

group = "dev.reddb"
version = "0.1.0"

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
    withJavadocJar()
}

repositories {
    mavenCentral()
}

dependencies {
    // Jackson for JSON encode/decode of handshake + RPC payloads.
    api("com.fasterxml.jackson.core:jackson-databind:2.17.2")

    // zstd-jni — lazily loaded; tests can run without zstd payloads.
    implementation("com.github.luben:zstd-jni:1.5.6-6")

    testImplementation(platform("org.junit:junit-bom:5.10.2"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testImplementation("org.junit.jupiter:junit-jupiter-params")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("passed", "skipped", "failed")
        showStandardStreams = false
    }
    // SmokeTest is gated on RED_SMOKE=1 — keep environment passthrough so
    // a CI runner can flip the bit without rewriting the build script.
    environment("RED_SMOKE", System.getenv("RED_SMOKE") ?: "0")
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            artifactId = "reddb-jvm"
            from(components["java"])
        }
    }
}
