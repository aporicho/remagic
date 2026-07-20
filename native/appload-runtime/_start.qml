import QtQuick 2.15
import QtQuick.Window 2.15
import QtQuick.Controls 2.15

import net.asivery.AppLoad 1.0

Window {
    id: window
    visible: true
    visibility: Window.FullScreen
    width: Screen.width
    height: Screen.height
    color: "white"
    title: qsTr("Remagic")

    Loader {
        id: loader
        anchors.fill: parent
        source: "qrc:/appload/qml/appload.qml"
        active: true

        onLoaded: {
            loader.item.visible = true
            loader.item.virtualKeyboardRef = keyboardLoader
            loader.item.requestClose.connect(function() { RuntimeControl.requestShutdown() })
            // The socket is the daemon's readiness boundary. Do not expose it
            // until the manager QML and all runtime signal handlers exist.
            if (!RuntimeControl.start()) {
                console.error("Failed to start the Remagic runtime control socket")
                Qt.quit()
                return
            }
            const toStart = AppLoadEmuOnly.startApp
            if (toStart)
                AppLoadLauncher.requestLaunch(toStart, [], {}, false)
        }
    }

    Loader {
        id: keyboardLoader
        property var layout: null
        property var config: null
        source: "qrc:/appload/qml/virtualKeyboard/Keyboard.qml"
        active: false
        width: parent.width
        anchors.bottom: parent.bottom
        anchors.horizontalCenter: parent.horizontalCenter

        onLoaded: {
            keyboardLoader.item.rebuildKeyboard(keyboardLoader.layout,
                                                keyboardLoader.config)
        }
    }
}
