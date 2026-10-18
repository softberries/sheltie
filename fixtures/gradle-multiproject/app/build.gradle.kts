plugins {
    application
}

dependencies {
    implementation(project(":api"))
}

application {
    mainClass = "com.acme.shop.app.App"
}
