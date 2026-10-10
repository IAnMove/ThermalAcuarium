import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

BarWidget {
    id: root

    // El agua hierve en el acuario a partir de ~82 °C: el icono avisa igual en la barra.
    property bool isHot: false
    moduleName: "io.github.ianmove.thermalacuarium"
    property string tempText: "--°"

    // Carpeta del plugin: el lanzador portátil está ahí y no hace falta instalar nada. La primera
    // vez baja el binario de la release (sha256 comprobado) a `bin/`.
    readonly property string dir: decodeURIComponent(Qt.resolvedUrl(".").toString().replace(/^file:\/\//, "")).replace(/\/$/, "")
    function launch(extra) {
        if (root.bar)
            root.bar.run("bash '" + root.dir + "/thermalacuarium.sh'" + (extra ? " " + extra : ""));
    }

    implicitHeight: button.implicitHeight
    implicitWidth: button.implicitWidth

    Timer {
        interval: 2000
        repeat: true
        running: true
        triggeredOnStart: true

        onTriggered: tempCheck.running = true
    }

    Process {
        id: tempCheck

        command: ["sh", "-c", "for p in /sys/class/hwmon/hwmon*; do if [ -f \"$p/name\" ] && grep -qE 'k10temp|coretemp|acpitz' \"$p/name\" 2>/dev/null; then cat \"$p/temp1_input\" 2>/dev/null && break; fi; done"]

        stdout: StdioCollector {
            onStreamFinished: {
                let v = parseInt(text.trim());
                if (!isNaN(v) && v > 0) {
                    let c = Math.round(v / 1000);
                    root.tempText = c + "°";
                    root.isHot = (c >= 82);
                }
            }
        }
    }

    WidgetButton {
        id: button

        anchors.fill: parent
        bar: root.bar
        text: (root.isHot ? "🫧 " : "🐠 ") + root.tempText
        tooltipText: "ThermalAcuarium · Clic: abrir el acuario (la primera vez descarga la app) · Clic derecho: otra ventana"

        onPressed: function(btn) {
            if (btn === Qt.RightButton)
                root.launch("--new-instance");
            else
                root.launch("");
        }
    }
}
