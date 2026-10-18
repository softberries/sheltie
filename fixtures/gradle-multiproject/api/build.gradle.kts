plugins {
    `java-library`
}

dependencies {
    api(projects.core)
    testImplementation(libs.junit.jupiter)
}

tasks.test {
    useJUnitPlatform()
}
